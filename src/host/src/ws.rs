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
//! # Three tasks, and one owner of the queue at a time
//!
//! A turn streams notifications *while* it runs and must be cancellable
//! mid-flight, so the socket is split: a reader task hands frames over, a
//! writer task drains lines onto the wire, and a blocking connection loop
//! dispatches.
//!
//! The receiver of incoming frames is **moved into each job and returned**
//! rather than shared. That is the whole concurrency argument: between
//! frames the connection loop owns it; while a turn runs, the turn owns
//! it, and `rpc::serve_queued` drains `turn/cancel` and an in-band
//! `turn/answer` out of it exactly as it does over stdio. Two consumers
//! never exist, so nothing has to be locked and no frame can be taken by
//! the wrong reader.
//!
//! This is the same division `rpc::serve` gets for free by being
//! single-threaded — there, the loop is simply blocked inside `command`
//! while the turn reads. Here the loop is blocked on the job instead.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use jan_klod_core::AgentSession;

use crate::rpc::{self, Incoming, Served, Wire};
use crate::sessions::Agents;

/// Where a wire's bytes go: whole lines, to be sent as text frames.
///
/// `rpc` writes `{json}\n` per frame, so the newline is the frame
/// boundary — this splits on it rather than sending partial JSON, which a
/// client would have no way to reassemble.
struct Frames {
    lines: UnboundedSender<String>,
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
pub async fn serve_socket(socket: WebSocket, agents: Arc<Agents>) {
    let (mut sink, mut stream) = socket.split();
    let (outbox, mut outgoing) = unbounded_channel::<String>();
    let writing = tokio::spawn(async move {
        while let Some(line) = outgoing.recv().await {
            if sink.send(Message::text(line)).await.is_err() {
                return;
            }
        }
        let _ = sink.send(Message::Close(None)).await;
    });

    // Hands frames over and holds nothing else — the same division
    // `rpc::serve` makes for its pipe, and for the same reason: while a
    // turn runs, something other than the turn has to be reading.
    let (handover, incoming) = channel::<Incoming>();
    let reading = tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            let handed = match message {
                Message::Text(text) => handover.send(Incoming::Line(text.to_string())),
                // Not this protocol. Handed on as such rather than
                // dropped, because silence looks like a lost message.
                Message::Binary(_) => handover.send(Incoming::NotUtf8),
                Message::Close(_) => break,
                // The transport's own; `axum` answers pings itself.
                Message::Ping(_) | Message::Pong(_) => Ok(()),
            };
            if handed.is_err() {
                break;
            }
        }
    });

    let _ = tokio::task::spawn_blocking(move || connection(incoming, &outbox, &agents)).await;
    // The connection is over: the reader has nothing left to hand to, and
    // the writer stops when the outbox drops with `connection`.
    reading.abort();
    let _ = writing.await;
}

/// Dispatch frames until the client leaves or the handshake is refused.
///
/// Blocking, and deliberately: it owns the frame queue between frames and
/// lends it to each job, which is what keeps a running turn the only
/// reader while it runs.
fn connection(mut incoming: Receiver<Incoming>, outbox: &UnboundedSender<String>, agents: &Agents) {
    let mut negotiated = false;
    while let Ok(frame) = incoming.recv() {
        let line = match frame {
            Incoming::Line(line) => line,
            Incoming::NotUtf8 => {
                if outbox.send(rpc::not_text()).is_err() {
                    return;
                }
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }

        // Which agent serves this frame is the frame's own business: a
        // connection is not bound to a session, and a turn must run on
        // the agent that owns the session it is for (#229). Parsed twice
        // — once to route, once inside the job — because the job cannot
        // borrow the request it was routed by.
        let jobs = match rpc::parse(&line) {
            Ok(request) => rpc::session_of(&request.command)
                .map_or_else(|| agents.housekeeping(), |session| agents.of(session)),
            // Unparseable frames go to housekeeping, which answers them
            // with the same refusal any agent would.
            Err(_) => agents.housekeeping(),
        };
        let lines = outbox.clone();
        let was = negotiated;
        let served = jobs.run(move |agent: &mut AgentSession| {
            let wire = Wire::new(
                Rc::new(RefCell::new(Frames {
                    lines,
                    partial: Vec::new(),
                })),
                &incoming,
            );
            let mut negotiated = was;
            let served = match rpc::parse(&line) {
                Ok(request) => {
                    rpc::command(request.command, request.id, &mut negotiated, agent, &wire)
                }
                Err(refusal) => Served::Answer(refusal),
            };
            let closing = matches!(served, Served::Close(_));
            let (Served::Answer(answer) | Served::Close(answer)) = served;
            let _ = wire.write(&answer);
            // Handed back so the loop owns it again: exactly one reader of
            // this queue exists at any moment.
            (incoming, negotiated, closing)
        });
        match served {
            Ok((returned, flag, closing)) => {
                incoming = returned;
                negotiated = flag;
                if closing {
                    return;
                }
            }
            // The session is gone; nothing more can be answered here.
            Err(_) => return,
        }
    }
}
