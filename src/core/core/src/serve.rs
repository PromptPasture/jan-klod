//! Host-side inbound HTTP surface — resource-model REST API (Phase 11b).
//!
//! Routes (v0.1.0):
//!   GET  /health                   liveness probe
//!   GET  /sessions                 list session ids + previews
//!   POST /sessions                 create session → `{"id":"<id>"}`
//!   GET  /session/:id              transcript + metadata
//!   POST /session/:id/message      send a message; SSE or JSON response
//!
//! Synchronous/blocking (`tiny_http`) — one request is served at a time on the
//! thread that owns the `!Send` `AgentSession`.

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

// ─── Route dispatch ──────────────────────────────────────────────────────────

/// Serve one request from `server` through `agent`, then respond.
///
/// # Errors
/// Returns the underlying I/O error if the request cannot be received, read, or
/// answered.
pub fn serve_once(server: &Server, agent: &mut AgentSession) -> std::io::Result<()> {
    let mut request = server.recv()?;
    let method = request.method().clone();
    let url = request.url().to_string();
    // Strip query string for routing.
    let path = url.split('?').next().unwrap_or(&url);

    // GET /health
    if method == Method::Get && path == "/health" {
        return respond_json(request, health());
    }

    // GET /sessions
    if method == Method::Get && path == "/sessions" {
        return respond_json(request, handle_list_sessions(agent));
    }

    // POST /sessions
    if method == Method::Post && path == "/sessions" {
        return respond_json(request, handle_create_session());
    }

    // GET /session/:id
    if method == Method::Get {
        if let Some(id) = strip_prefix(path, "/session/") {
            if !id.contains('/') {
                return respond_json(request, handle_get_session(agent, id));
            }
        }
    }

    // POST /session/:id/message
    if method == Method::Post {
        if let Some(rest) = strip_prefix(path, "/session/") {
            if let Some(id) = rest.strip_suffix("/message") {
                let wants_sse = accepts_event_stream(&request);
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body)?;
                return if wants_sse {
                    serve_message_sse(request, agent, id, &body)
                } else {
                    respond_json(request, handle_message(agent, id, &body))
                };
            }
        }
    }

    respond_json(request, error_reply(404, "not found"))
}

// ─── Route handlers ──────────────────────────────────────────────────────────

/// `GET /health` — liveness probe.
#[must_use]
pub fn health() -> Reply {
    Reply {
        status: 200,
        body: serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") }).to_string(),
    }
}

/// `GET /sessions` — list all session ids with a preview of the first turn.
fn handle_list_sessions(agent: &AgentSession) -> Reply {
    let sessions: Vec<serde_json::Value> = agent
        .list_sessions()
        .into_iter()
        .map(|id| {
            let preview = agent
                .transcript(&id)
                .into_iter()
                .next()
                .and_then(|e| {
                    let v: serde_json::Value = serde_json::from_str(&e.value).ok()?;
                    Some(v.get("user")?.as_str()?.chars().take(80).collect::<String>())
                })
                .unwrap_or_default();
            serde_json::json!({ "id": id, "preview": preview })
        })
        .collect();
    Reply { status: 200, body: serde_json::json!({ "sessions": sessions }).to_string() }
}

/// `POST /sessions` — allocate a new session id.
fn handle_create_session() -> Reply {
    let id = new_session_id();
    Reply { status: 201, body: serde_json::json!({ "id": id }).to_string() }
}

/// `GET /session/:id` — return transcript + metadata.
fn handle_get_session(agent: &AgentSession, id: &str) -> Reply {
    let entries: Vec<serde_json::Value> = agent
        .transcript(id)
        .into_iter()
        .filter_map(|e| serde_json::from_str(&e.value).ok())
        .collect();
    Reply {
        status: 200,
        body: serde_json::json!({ "id": id, "turns": entries }).to_string(),
    }
}

/// `POST /session/:id/message` — blocking (non-SSE) turn.
fn handle_message(agent: &mut AgentSession, session: &str, body: &str) -> Reply {
    let message = match parse_message_body(body) {
        Ok(m) => m,
        Err(err) => return error_reply(400, &err),
    };
    match agent.run(session, &message) {
        RunResult::Answered { text, agentic } => Reply {
            status: 200,
            body: serde_json::json!({ "answer": text, "agentic": agentic }).to_string(),
        },
        RunResult::Failed(reason) => error_reply(502, &reason),
    }
}

/// `POST /session/:id/message` (SSE variant) — stream turn events.
fn serve_message_sse(
    request: Request,
    agent: &mut AgentSession,
    session: &str,
    body: &str,
) -> std::io::Result<()> {
    let mut writer = request.into_writer();
    writer.write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )?;

    let message = match parse_message_body(body) {
        Ok(m) => m,
        Err(err) => {
            let _ =
                write_frame(&mut writer, "error", &serde_json::json!({ "error": err }).to_string());
            return Ok(());
        }
    };

    let mut sink = SseSink { writer: &mut writer, live: true };
    if let RunResult::Failed(reason) = agent.run_streaming_headless(&mut sink, session, &message) {
        let _ = write_frame(
            &mut writer,
            "error",
            &serde_json::json!({ "error": reason }).to_string(),
        );
    }
    Ok(())
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Whether the client asked for an SSE stream (`Accept: text/event-stream`).
fn accepts_event_stream(request: &Request) -> bool {
    request.headers().iter().any(|h| {
        h.field.as_str().as_str().eq_ignore_ascii_case("accept")
            && h.value.as_str().contains("text/event-stream")
    })
}

/// Write a single JSON reply and finish the request.
fn respond_json(request: Request, reply: Reply) -> std::io::Result<()> {
    request.respond(
        Response::from_string(reply.body)
            .with_status_code(reply.status)
            .with_header(json_content_type()),
    )
}

/// An [`EventSink`] that writes each event as an SSE frame to the socket.
struct SseSink<'a> {
    writer: &'a mut dyn Write,
    live: bool,
}

impl EventSink for SseSink<'_> {
    fn emit(&mut self, event: &Event) -> Flow {
        if !self.live {
            return Flow::Stop;
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
            self.live = false;
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

/// Extract `message` from a `POST /session/:id/message` body (`{"message":"..."}`).
fn parse_message_body(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let message = value
        .get("message")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing string field `message`")?;
    Ok(message.to_string())
}

/// Strip `prefix` from `s`, returning the remainder if it matches.
fn strip_prefix<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    s.strip_prefix(prefix)
}

fn error_reply(status: u16, message: &str) -> Reply {
    Reply { status, body: serde_json::json!({ "error": message }).to_string() }
}

fn json_content_type() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .unwrap_or_else(|()| Header::from_bytes(&b"X-Content"[..], &b"json"[..]).expect("static header"))
}

/// Generate a random hex session id (16 hex chars, using stdlib only).
fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Mix timestamp nanos with a counter to get unique ids without a rand dep.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("{:08x}{:08x}", ts ^ (count << 17), count.wrapping_mul(0x9e37_79b9))
}

/// Serve requests forever (the accept loop). Blocks the calling thread.
///
/// # Errors
/// Propagates the first I/O error from [`serve_once`].
pub fn serve(server: &Server, agent: &mut AgentSession) -> std::io::Result<()> {
    loop {
        serve_once(server, agent)?;
    }
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
