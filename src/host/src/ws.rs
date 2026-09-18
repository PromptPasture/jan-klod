//! `GET /ws` — the client protocol over one connection (#226, closing #43).
//!
//! Same protocol as stdio, same dispatch, same token rule. What a
//! WebSocket adds over REST + SSE is one connection instead of two, and a
//! graceful in-band `turn/cancel` rather than a stream teardown.
//!
//! # It reuses `rpc`, it does not re-implement it
//!
//! `rpc::command` is generic over its writer and takes the session **per
//! message** (`rpc.rs`), which is exactly the unit a socket needs: each
//! frame becomes one job on the session's thread. So the answers here are
//! the stdio answers because they are produced by the same function, not
//! because a second implementation agrees with it today.
//!
//! `rpc::serve` would have been the wrong thing to reuse. It holds
//! `&mut AgentSession` for a whole connection, which on a shared surface
//! would let one socket client stall every REST caller for as long as it
//! stayed connected.
//!
//! # Sequential, for now
//!
//! One frame in, its answers out, then the next. That is enough for every
//! command that is not a turn, and it keeps the socket free of a second
//! writer. A turn streams notifications *while* running and must be
//! cancellable mid-flight, which needs a reader and a writer at once —
//! that is the next slice's problem, and the reason this module does not
//! split the socket yet.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::mpsc::{channel, Sender};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};

use jan_klod_core::AgentSession;

use crate::rpc::{self, Incoming, Served, Wire};
use crate::session_thread::Jobs;

/// Where a wire's bytes go: whole lines, to be sent as text frames.
///
/// `rpc` writes `{json}\n` per frame, so the newline is the frame
/// boundary — this splits on it rather than sending partial JSON, which a
/// client would have no way to reassemble.
struct Frames {
    lines: Sender<String>,
    partial: Vec<u8>,
}

impl Write for Frames {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        for byte in bytes {
            if *byte == b'\n' {
                let line = String::from_utf8_lossy(&self.partial).into_owned();
                self.partial.clear();
                // A closed receiver means the connection is gone. Reported
                // as a write error, which is what `rpc` already handles.
                self.lines
                    .send(line)
                    .map_err(|_| std::io::Error::other("the socket is closed"))?;
            } else {
                self.partial.push(*byte);
            }
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Serve one client until it hangs up or the handshake is refused.
pub async fn serve_socket(mut socket: WebSocket, jobs: Arc<Jobs<AgentSession>>) {
    // Carried across frames: the protocol is negotiated once per
    // connection, and `rpc` keeps the same flag for the same reason.
    let mut negotiated = false;
    while let Some(Ok(message)) = socket.recv().await {
        let line = match message {
            Message::Text(text) => text.to_string(),
            // A binary frame is not this protocol. Said rather than
            // ignored, because silence looks like a lost message.
            Message::Binary(_) => {
                let refusal = rpc::not_text();
                let _ = socket.send(Message::text(refusal)).await;
                continue;
            }
            Message::Close(_) => break,
            // Ping and pong are the transport's own; `axum` answers pings.
            Message::Ping(_) | Message::Pong(_) => continue,
        };
        if line.trim().is_empty() {
            continue;
        }

        let jobs = Arc::clone(&jobs);
        let was = negotiated;
        let served = tokio::task::spawn_blocking(move || {
            let (lines, written) = channel();
            let mut negotiated = was;
            let closing = jobs.run(move |agent: &mut AgentSession| {
                // Both live only for this frame: the wire's writer is the
                // channel above, and nothing polls `incoming` until a turn
                // does (the next slice).
                let (_unused, incoming) = channel::<Incoming>();
                let wire = Wire::new(
                    Rc::new(RefCell::new(Frames {
                        lines,
                        partial: Vec::new(),
                    })),
                    &incoming,
                );
                let served = match rpc::parse(&line) {
                    Ok(request) => {
                        rpc::command(request.command, request.id, &mut negotiated, agent, &wire)
                    }
                    Err(refusal) => Served::Answer(refusal),
                };
                let closing = matches!(served, Served::Close(_));
                let answer = match served {
                    Served::Answer(answer) | Served::Close(answer) => answer,
                };
                let _ = wire.write(&answer);
                (closing, negotiated)
            });
            (closing, written.into_iter().collect::<Vec<String>>())
        })
        .await;

        let Ok((closing, answers)) = served else {
            break;
        };
        for answer in answers {
            if socket.send(Message::text(answer)).await.is_err() {
                return;
            }
        }
        // A refused handshake hangs up, and so does a session that has
        // gone: in both cases nothing more can be answered on this socket.
        match closing {
            Ok((false, flag)) => negotiated = flag,
            Ok((true, _)) | Err(_) => break,
        }
    }
    let _ = socket.send(Message::Close(None)).await;
}
