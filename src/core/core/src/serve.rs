//! Host-side inbound HTTP surface (Phase 3 Slice 3b).
//!
//! A small **synchronous** REST endpoint that drives the thin loop: an external
//! client `POST`s a turn and gets the answer back. It is host-side (not a wasm
//! guest) by design — the loop it drives is host mechanism, and the wasip2 sandbox
//! grants no inbound sockets. Synchronous/blocking (`tiny_http`) fits the sync loop
//! and the `!Send` Wasmtime-backed [`AgentSession`]: one request is served at a
//! time, in the thread that owns the session.
//!
//! v1 routes: `POST /turn` request→response JSON (`{ "session", "message" }` →
//! `{ "answer", "agentic" }`) and `GET /health` (liveness for the blue/green
//! supervisor). Server-Sent-Events streaming of `next-event` is a follow-up once
//! the streaming run-handle lands.

use std::io::Write;

use tiny_http::{Header, Method, Request, Response, Server};

use crate::conductor::{Event, EventSink, Flow, RunResult};
use crate::AgentSession;

/// A ready HTTP reply: status code + JSON body.
pub struct Reply {
    /// HTTP status code.
    pub status: u16,
    /// JSON body.
    pub body: String,
}

/// Handle one turn request body, driving the loop and returning the JSON reply.
///
/// The body is `{ "session"?, "message" }`; `session` defaults to `"default"`.
/// Pure over the body, so it is unit-testable without a socket.
#[must_use]
pub fn handle_turn(agent: &mut AgentSession, body: &str) -> Reply {
    let (session, message) = match parse_turn(body) {
        Ok(parsed) => parsed,
        Err(err) => return error_reply(400, &err),
    };
    match agent.run(&session, &message) {
        RunResult::Answered { text, agentic } => Reply {
            status: 200,
            body: serde_json::json!({ "answer": text, "agentic": agentic }).to_string(),
        },
        RunResult::Failed(reason) => error_reply(502, &reason),
    }
}

/// Serve one request from `server` through `agent`, then respond. Returns after a
/// single request (the unit the accept loop repeats), so a test can drive exactly
/// one round-trip.
///
/// # Errors
/// Returns the underlying I/O error if the request cannot be received, read, or
/// answered.
pub fn serve_once(server: &Server, agent: &mut AgentSession) -> std::io::Result<()> {
    let mut request = server.recv()?;
    let method = request.method().clone();
    let url = request.url().to_string();

    // GET /health — liveness.
    if method == Method::Get && url.starts_with("/health") {
        return respond_json(request, health());
    }
    if method != Method::Post {
        return respond_json(request, error_reply(405, "use POST /turn or GET /health"));
    }

    // POST /turn — read the body, then either stream (SSE) or reply once.
    let wants_sse = accepts_event_stream(&request);
    let mut body = String::new();
    request.as_reader().read_to_string(&mut body)?;

    if wants_sse {
        serve_turn_sse(request, agent, &body)
    } else {
        respond_json(request, handle_turn(agent, &body))
    }
}

/// Whether the client asked for an SSE stream (`Accept: text/event-stream`).
fn accepts_event_stream(request: &Request) -> bool {
    request.headers().iter().any(|h| {
        h.field.as_str().as_str().eq_ignore_ascii_case("accept")
            && h.value.as_str().contains("text/event-stream")
    })
}

/// Write a single JSON reply and finish the request.
fn respond_json(request: Request, reply: Reply) -> std::io::Result<()> {
    let response = Response::from_string(reply.body)
        .with_status_code(reply.status)
        .with_header(json_content_type());
    request.respond(response)
}

/// Stream a turn as Server-Sent Events: take raw access to the socket, write the
/// SSE status line + headers, then push one frame per [`Event`] as the turn runs
/// (the conductor emits synchronously on this thread). A terminal `done` frame
/// carries the authoritative answer; a failed turn ends with an `error` frame.
fn serve_turn_sse(request: Request, agent: &mut AgentSession, body: &str) -> std::io::Result<()> {
    let mut writer = request.into_writer();
    writer.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )?;

    let (session, message) = match parse_turn(body) {
        Ok(parsed) => parsed,
        Err(err) => {
            let _ = write_frame(&mut writer, "error", &serde_json::json!({ "error": err }).to_string());
            return Ok(());
        }
    };

    let mut sink = SseSink { writer: &mut writer, live: true };
    if let RunResult::Failed(reason) = agent.run_streaming_headless(&mut sink, &session, &message) {
        // The conductor already emitted a Warning; add an explicit terminal error.
        let _ = write_frame(&mut writer, "error", &serde_json::json!({ "error": reason }).to_string());
    }
    Ok(())
}

/// An [`EventSink`] that writes each event as an SSE frame to the socket. Best
/// effort: once a write fails (client gone) it stops.
struct SseSink<'a> {
    writer: &'a mut dyn Write,
    live: bool,
}

impl EventSink for SseSink<'_> {
    fn emit(&mut self, event: &Event) -> Flow {
        if !self.live {
            return Flow::Stop; // client gone — cancel the turn
        }
        let (kind, data) = match event {
            Event::TextDelta(text) => ("delta", serde_json::json!({ "text": text })),
            Event::ToolInvoked(call) => {
                ("tool", serde_json::json!({ "id": call.id, "name": call.name }))
            }
            Event::ToolResult(outcome) => (
                "tool-result",
                serde_json::json!({ "id": outcome.tool_call_id, "content": outcome.content }),
            ),
            Event::Warning(message) => ("warning", serde_json::json!({ "message": message })),
            Event::Done { text, agentic } => {
                ("done", serde_json::json!({ "answer": text, "agentic": agentic }))
            }
        };
        if write_frame(self.writer, kind, &data.to_string()).is_err() {
            self.live = false; // client disconnected — stop writing
            return Flow::Stop;
        }
        Flow::Continue
    }
}

/// Write one SSE frame: `event: <kind>\ndata: <json>\n\n`, flushed.
fn write_frame(writer: &mut dyn Write, kind: &str, data: &str) -> std::io::Result<()> {
    write!(writer, "event: {kind}\ndata: {data}\n\n")?;
    writer.flush()
}

/// Extract `(session, message)` from a turn request body (`session` defaults to
/// `"default"`).
fn parse_turn(body: &str) -> Result<(String, String), String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|err| format!("invalid JSON body: {err}"))?;
    let message = value
        .get("message")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing string field `message`")?;
    let session = value
        .get("session")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("default");
    Ok((session.to_string(), message.to_string()))
}

/// Serve requests forever (the accept loop). Blocks the calling thread; one turn
/// is handled at a time.
///
/// # Errors
/// Propagates the first I/O error from [`serve_once`].
pub fn serve(server: &Server, agent: &mut AgentSession) -> std::io::Result<()> {
    loop {
        serve_once(server, agent)?;
    }
}

/// The liveness reply for `GET /health` — a cheap 200 the supervisor probes after
/// a blue/green flip to confirm the swapped core booted and can serve.
#[must_use]
pub fn health() -> Reply {
    Reply {
        status: 200,
        body: serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }).to_string(),
    }
}

fn error_reply(status: u16, message: &str) -> Reply {
    Reply {
        status,
        body: serde_json::json!({ "error": message }).to_string(),
    }
}

fn json_content_type() -> Header {
    // Infallible for this constant; fall back to an empty header if it ever isn't.
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .unwrap_or_else(|()| Header::from_bytes(&b"X-Content"[..], &b"json"[..]).expect("static header"))
}

#[cfg(test)]
mod tests {
    use super::health;

    #[test]
    fn health_reports_ok_with_a_version() {
        let reply = health();
        assert_eq!(reply.status, 200);
        assert!(reply.body.contains("\"status\":\"ok\""), "{}", reply.body);
        assert!(reply.body.contains(env!("CARGO_PKG_VERSION")), "{}", reply.body);
    }
}
