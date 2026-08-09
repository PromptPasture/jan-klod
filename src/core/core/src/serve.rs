//! Host-side inbound HTTP surface — resource-model REST API (Phase 11b).
//!
//! Routes (v0.1.0):
//!   GET  /health                   liveness probe
//!   GET  /sessions                 list session ids + previews
//!   POST /sessions                 create session → `{"id":"<id>"}`
//!   GET  /session/:id              transcript + metadata
//!   POST /session/:id/message      send a message; SSE or JSON response
//!   POST /session/:id/answer       answer a pending confirmation
//!
//! Synchronous/blocking (`tiny_http`) — one request is served at a time on the
//! thread that owns the `!Send` `AgentSession`.
//!
//! ## Answering a mid-turn confirmation without threads
//!
//! An interceptor can stop a turn to ask the user something (the permission gate
//! does exactly this). The loop is synchronous, so the turn *blocks* inside
//! `Driver::ask` — which means the answer has to arrive on a different request
//! while this one is still open, and a single-threaded server cannot accept it.
//!
//! Rather than make `AgentSession` `Send` and put turns on worker threads, the
//! waiting driver **serves the socket itself**: it emits a `prompt` SSE frame,
//! then keeps calling `Server::recv_timeout` until a matching
//! `POST /session/:id/answer` arrives, replying `409` to anything else that comes
//! in meanwhile. The concurrency stays exactly where it was — one request at a
//! time — and the sync-Wasmtime, no-`tokio` posture of the rest of the core holds.
//! An unanswered prompt times out at [`ANSWER_TIMEOUT`] and takes the prompt's own
//! default, which for the permission gate is a denial.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tiny_http::{Header, Method, Request, Response, Server};

use crate::conductor::{Event, EventSink, Flow, RunResult};
use crate::intercept::{Driver, UserPrompt};
use crate::AgentSession;

/// How long a turn waits for a confirmation before giving up and taking the
/// prompt's default answer. Long enough for a person to read and decide; short
/// enough that a client that vanished mid-prompt cannot pin the server open.
#[allow(clippy::duration_suboptimal_units)] // no stable `Duration::from_mins`
const ANSWER_TIMEOUT: Duration = Duration::from_secs(180);

/// The response socket, shared between the event sink and the prompt driver —
/// both write frames to the same stream while the turn runs.
type SharedWriter = Rc<RefCell<Box<dyn Write + Send>>>;

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
    serve_once_authed(server, agent, None)
}

/// Serve one request, requiring `Bearer <token>` when `token` is `Some`.
///
/// # Errors
/// Returns the underlying I/O error if the request cannot be received, read, or
/// answered.
pub fn serve_once_authed(
    server: &Server,
    agent: &mut AgentSession,
    token: Option<&str>,
) -> std::io::Result<()> {
    let mut request = server.recv()?;
    let method = request.method().clone();
    let url = request.url().to_string();
    // Strip query string for routing.
    let path = url.split('?').next().unwrap_or(&url);

    // Auth first, before any route can act. `/health` stays open: it carries no
    // session data and the blue/green supervisor probes it without credentials.
    if !authorised(&request, token, path) {
        return respond_json(
            request,
            error_reply(401, "missing or invalid bearer token"),
        );
    }

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

    // POST /session/:id/answer — only meaningful while a turn is waiting; the
    // waiting driver intercepts it there. Reaching the main loop means nothing
    // asked, so say that rather than 404-ing on a route that does exist.
    if method == Method::Post {
        if let Some(rest) = strip_prefix(path, "/session/") {
            if rest.strip_suffix("/answer").is_some() {
                return respond_json(request, error_reply(409, "no confirmation is pending"));
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
                    serve_message_sse(server, request, agent, id, &body, token)
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
    server: &Server,
    request: Request,
    agent: &mut AgentSession,
    session: &str,
    body: &str,
    token: Option<&str>,
) -> std::io::Result<()> {
    let writer: SharedWriter = Rc::new(RefCell::new(request.into_writer()));
    writer.borrow_mut().write_all(
        b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
    )?;

    let message = match parse_message_body(body) {
        Ok(m) => m,
        Err(err) => {
            let _ = write_frame(
                &mut *writer.borrow_mut(),
                "error",
                &serde_json::json!({ "error": err }).to_string(),
            );
            return Ok(());
        }
    };

    let mut sink = SseSink { writer: Rc::clone(&writer), live: true };
    let mut driver = PromptDriver {
        server,
        writer: Rc::clone(&writer),
        session: session.to_string(),
        token,
    };
    if let RunResult::Failed(reason) =
        agent.run_streaming_with_driver(&mut driver, &mut sink, session, &message)
    {
        let _ = write_frame(
            &mut *writer.borrow_mut(),
            "error",
            &serde_json::json!({ "error": reason }).to_string(),
        );
    }
    Ok(())
}

/// Puts an interceptor's question to the client over the open SSE stream and
/// waits, on this same thread, for the answer to arrive as its own request.
struct PromptDriver<'a> {
    server: &'a Server,
    writer: SharedWriter,
    session: String,
    /// The bearer token, if one is configured.
    ///
    /// This driver serves the socket itself while a turn is parked, so it does
    /// **not** pass through `serve_once_authed` and has to check the token on its
    /// own. Missing that is how the answer route — the one that approves a write
    /// or a command — became the single unguarded endpoint the moment auth was
    /// added everywhere else.
    token: Option<&'a str>,
}

impl Driver for PromptDriver<'_> {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        let payload = serde_json::json!({
            "question": prompt.question,
            "options": prompt.options,
            "default": prompt.default_answer,
            "session": self.session,
        });
        if write_frame(&mut *self.writer.borrow_mut(), "prompt", &payload.to_string()).is_err() {
            // The client is gone; nobody can answer, so take the safe default.
            return prompt.default_answer.clone();
        }
        self.wait_for_answer().unwrap_or_else(|| prompt.default_answer.clone())
    }
}

impl PromptDriver<'_> {
    /// Serve the socket until this session's answer arrives, or the wait expires.
    ///
    /// Anything else received meanwhile is refused with `409` rather than queued:
    /// the agent is mid-turn and single-threaded, and a client that is told
    /// "busy" can retry, while one left hanging cannot.
    fn wait_for_answer(&self) -> Option<String> {
        let deadline = Instant::now() + ANSWER_TIMEOUT;
        let route = format!("/session/{}/answer", self.session);
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let mut request = self.server.recv_timeout(remaining).ok().flatten()?;
            let path = request.url().split('?').next().unwrap_or("").to_string();
            if !authorised(&request, self.token, &path) {
                // Drain before replying: the body is still in the socket, and
                // answering a POST without consuming it leaves the connection
                // mid-message — the client then waits for bytes that never come.
                let mut discard = String::new();
                let _ = request.as_reader().read_to_string(&mut discard);
                // Refuse and keep waiting: an unauthenticated caller must not be
                // able to answer a permission prompt, nor to cancel one by
                // consuming the wait.
                let _ = respond_json(
                    request,
                    error_reply(401, "missing or invalid bearer token"),
                );
                continue;
            }
            if *request.method() == Method::Post && path == route {
                let mut body = String::new();
                if request.as_reader().read_to_string(&mut body).is_err() {
                    let _ = respond_json(request, error_reply(400, "unreadable body"));
                    continue;
                }
                match parse_answer_body(&body) {
                    Ok(answer) => {
                        let _ = respond_json(
                            request,
                            Reply { status: 200, body: r#"{"accepted":true}"#.to_string() },
                        );
                        return Some(answer);
                    }
                    Err(err) => {
                        let _ = respond_json(request, error_reply(400, &err));
                    }
                }
            } else {
                let _ = respond_json(
                    request,
                    error_reply(409, "the agent is waiting for an answer to a confirmation"),
                );
            }
        }
    }
}

// ─── Internal helpers ────────────────────────────────────────────────────────

/// Whether a request may proceed.
///
/// `None` means no token is configured, and everything is allowed — the
/// historical behaviour, kept because the default bind is loopback and requiring
/// a secret to talk to your own machine would be friction without a threat. When
/// a token *is* configured it is required everywhere except `/health`, which
/// leaks nothing and is what the supervisor probes.
///
/// Comparison is length-then-bytes rather than `==` on `&str` only in the sense
/// that it compares the whole string every time; this is a local single-user
/// surface, not a service where timing analysis of a bearer token is the
/// realistic attack. The realistic attack is that there was no token at all.
fn authorised(request: &Request, token: Option<&str>, path: &str) -> bool {
    let Some(expected) = token else { return true };
    if path == "/health" {
        return true;
    }
    request
        .headers()
        .iter()
        .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("authorization"))
        .and_then(|h| {
            let value = h.value.as_str();
            value.strip_prefix("Bearer ").or_else(|| value.strip_prefix("bearer "))
        })
        .is_some_and(|presented| presented.trim() == expected)
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
    request.respond(
        Response::from_string(reply.body)
            .with_status_code(reply.status)
            .with_header(json_content_type()),
    )
}

/// An [`EventSink`] that writes each event as an SSE frame to the socket.
struct SseSink {
    writer: SharedWriter,
    live: bool,
}

impl EventSink for SseSink {
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
        if write_frame(&mut *self.writer.borrow_mut(), kind, &data.to_string()).is_err() {
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

/// Extract `answer` from a `POST /session/:id/answer` body (`{"answer":"yes"}`).
fn parse_answer_body(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let answer = value
        .get("answer")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing string field `answer`")?;
    Ok(answer.to_string())
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
    serve_authed(server, agent, None)
}

/// Serve until killed, requiring `Bearer <token>` when one is configured.
///
/// # Errors
/// Propagates the first I/O error from the accept loop.
pub fn serve_authed(
    server: &Server,
    agent: &mut AgentSession,
    token: Option<&str>,
) -> std::io::Result<()> {
    loop {
        serve_once_authed(server, agent, token)?;
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
