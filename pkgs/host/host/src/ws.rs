//! `GET /ws` — the client protocol over one connection (#226, closing #43).
//! Reuses `rpc::command` per frame, so answers match stdio. The frame receiver is moved into each job and returned,
//! so exactly one reader exists at any moment and nothing is locked.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt as _, StreamExt as _};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use jk_agent::AgentSession;

use crate::rpc::{self, Incoming, Served, Wire};
use crate::sessions::Agents;

/// Where a wire's bytes go: whole lines sent as text frames (`rpc` newline-terminates each frame).
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

    let (handover, incoming) = channel::<Incoming>();
    let reading = tokio::spawn(async move {
        while let Some(Ok(message)) = stream.next().await {
            let handed = match message {
                Message::Text(text) => handover.send(Incoming::Line(text.to_string())),
                Message::Binary(_) => handover.send(Incoming::NotUtf8),
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => Ok(()),
            };
            if handed.is_err() {
                break;
            }
        }
    });

    let _ = tokio::task::spawn_blocking(move || connection(incoming, &outbox, &agents)).await;
    reading.abort();
    let _ = writing.await;
}

/// Dispatch frames until the client leaves or the handshake is refused.
/// Blocking so a running turn is the only reader of the queue.
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

        let jobs = match rpc::parse(&line) {
            Ok(request) => rpc::session_of(&request.command)
                .map_or_else(|| agents.housekeeping(), |session| agents.of(session)),
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
            Err(_) => return,
        }
    }
}
