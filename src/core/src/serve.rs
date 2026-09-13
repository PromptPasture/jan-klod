//! REST API surface (v0.1.0).
//!
//! Routes: GET /health, GET /sessions, POST /sessions, GET /session/:id, POST
//! /session/:id/message (SSE or JSON), answer, fork. Synchronous/blocking
//! (`tiny_http`): one request at a time.
//!
//! Mid-turn confirmations without threads: interceptors block in `Driver::ask`;
//! the waiting driver serves the socket itself via `recv_timeout` loop until
//! `POST /session/:id/answer` arrives (409 to others). Unanswered prompts timeout
//! at [`DEFAULT_ANSWER_TIMEOUT`], taking the prompt's default.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::time::{Duration, Instant};

use tiny_http::{Header, Method, Request, Response, Server};

use crate::conductor::{Event, EventSink, Flow, RunResult};
use crate::intercept::{Driver, Message, Role, UserPrompt};
use crate::AgentSession;

/// How long a turn waits for a confirmation before giving up and taking the
/// prompt's default answer. Long enough for a person to read and decide; short
/// enough that a client that vanished mid-prompt cannot pin the server open.
#[allow(clippy::duration_suboptimal_units)] // no stable `Duration::from_mins`
const DEFAULT_ANSWER_TIMEOUT: Duration = Duration::from_secs(180);

/// Overrides [`DEFAULT_ANSWER_TIMEOUT`], in seconds. Read per wait, not cached,
/// so a test can set it per process; three minutes is right for humans, wrong
/// for tests (unanswered tests still pass with default answer, just slowly).
const TIMEOUT_ENV: &str = "JK_ANSWER_TIMEOUT_SECS";

/// How often the wait pokes the event stream while parked. A vanished client
/// (post-write) looks fine until FIN is processed; each tick writes an SSE
/// comment: dead peers surface as write errors within one interval, and live
/// streams get keepalive bytes. Comments (`:` lines) are protocol no-ops.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// The configured confirmation timeout. Exported so both REST and stdio
/// transports use the same window; varying by transport would surprise users.
#[must_use]
pub fn answer_timeout() -> Duration {
    std::env::var(TIMEOUT_ENV)
        .ok()
        .and_then(|raw| raw.parse::<u64>().ok())
        .map_or(DEFAULT_ANSWER_TIMEOUT, Duration::from_secs)
}

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

    // Auth first. `/health` stays open: no session data, supervisor probes unauthed.
    if !authorised(&request, token, path) {
        return respond_json(request, error_reply(401, "missing or invalid bearer token"));
    }

    // GET / and its one asset — the web client, embedded above.
    // Two exact paths, not a prefix: `starts_with("/")` would match every route
    // on this surface, and a static-file handler that shadows the API is a
    // worse bug than no web client at all.
    if method == Method::Get && path == "/" {
        return respond_asset(request, WEB_INDEX, "text/html; charset=utf-8");
    }
    if method == Method::Get && path == "/app.js" {
        return respond_asset(request, WEB_APP_JS, "text/javascript; charset=utf-8");
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

    // POST /session/:id/answer — intercepted by waiting driver. Reaching here
    // means no turn is waiting; return 409, not 404.
    if method == Method::Post {
        if let Some(rest) = strip_prefix(path, "/session/") {
            if rest.strip_suffix("/answer").is_some() {
                return respond_json(request, error_reply(409, "no confirmation is pending"));
            }
        }
    }

    // POST /session/:id/fork
    if method == Method::Post {
        if let Some(rest) = strip_prefix(path, "/session/") {
            if let Some(id) = rest.strip_suffix("/fork") {
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body)?;
                let reply = handle_fork_session(agent, id, &body);
                return respond_json(request, reply);
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
        body: serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") })
            .to_string(),
    }
}

/// Every session with a preview, as any transport reports it.
///
/// Shared across transports so a client that lists sessions over stdio and over
/// REST must render one shape, not two.
#[must_use]
pub fn sessions_payload(agent: &AgentSession) -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = agent
        .list_sessions()
        .into_iter()
        .map(|id| {
            // First user message (readable in picker). From projection, not log
            // row 1, as row 1 need not be a user message (steered/interrupted turns).
            let preview = agent
                .transcript(&id)
                .into_iter()
                .find(|message| message.role == Role::User)
                .map(|message| message.content.chars().take(80).collect::<String>())
                .unwrap_or_default();
            serde_json::json!({ "id": id, "preview": preview })
        })
        .collect();
    serde_json::json!({ "sessions": sessions })
}

/// `GET /sessions` — list all session ids with a preview of the first turn.
fn handle_list_sessions(agent: &AgentSession) -> Reply {
    Reply {
        status: 200,
        body: sessions_payload(agent).to_string(),
    }
}

/// `POST /sessions` — allocate a new session id.
fn handle_create_session() -> Reply {
    let id = new_session_id();
    Reply {
        status: 201,
        body: serde_json::json!({ "id": id }).to_string(),
    }
}

/// `POST /session/:id/fork` — start a new session from a prefix. Child id
/// generated here (like POST /sessions) to avoid collisions; client-picked ids
/// could collide with live sessions, causing confusing 500s.
fn handle_fork_session(agent: &AgentSession, id: &str, body: &str) -> Reply {
    let at_seq = match parse_at_seq(body) {
        Ok(at_seq) => at_seq,
        Err(message) => return error_reply(400, &message),
    };
    match fork(agent, id, at_seq) {
        Forked::Created(payload) => Reply {
            status: 201,
            body: payload.to_string(),
        },
        Forked::Empty(message) => error_reply(404, &message),
        Forked::Failed(message) => error_reply(500, &message),
    }
}

/// What a fork did, in the three ways it can end.
///
/// Three variants rather than `Result<Value, String>` because the two failures
/// are not the same: the caller asked for something missing (`404`) or the store
/// broke (`500`). Collapsing them would make a mistyped `at-seq` look like
/// a broken database.
pub enum Forked {
    /// The child session, as `{"id": …, "copied": …}`.
    Created(serde_json::Value),
    /// Nothing was copied, so no fork was made.
    Empty(String),
    /// The store refused.
    Failed(String),
}

/// Fork `id` at `at_seq` into a freshly generated child session. Generated id
/// avoids collisions; client-picked ids could collide with live sessions.
#[must_use]
pub fn fork(agent: &AgentSession, id: &str, at_seq: u64) -> Forked {
    let child = new_session_id();
    match agent.fork_session(id, at_seq, &child) {
        // Empty fork (no events at seq) usually means wrong `at-seq` or session id;
        // report it rather than silently returning a non-fork.
        Ok(0) => Forked::Empty(format!(
            "session `{id}` has no events at or before seq {at_seq}"
        )),
        Ok(copied) => Forked::Created(serde_json::json!({ "id": child, "copied": copied })),
        Err(err) => Forked::Failed(format!("fork failed: {err}")),
    }
}

/// Extract `at-seq` from a `POST /session/:id/fork` body (`{"at-seq":3}`).
fn parse_at_seq(body: &str) -> Result<u64, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    value
        .get("at-seq")
        .ok_or("missing field `at-seq`")?
        .as_u64()
        .ok_or_else(|| "`at-seq` must be a non-negative whole number".to_owned())
}

/// One session's message list. Shared across transports like [`sessions_payload`].
#[must_use]
pub fn session_payload(agent: &AgentSession, id: &str) -> serde_json::Value {
    let messages: Vec<serde_json::Value> = agent
        .placed_transcript(id)
        .iter()
        .map(|(seq, message)| as_json(*seq, message))
        .collect();
    serde_json::json!({ "id": id, "messages": messages })
}

/// `GET /session/:id` — return transcript + metadata.
fn handle_get_session(agent: &AgentSession, id: &str) -> Reply {
    Reply {
        status: 200,
        body: session_payload(agent, id).to_string(),
    }
}

/// One message as this surface serves it.
///
/// `messages`, not the `turns` this used to return. A turn was a `{user, answer}`
/// pair because that's what the transcript row held; now the session projects from
/// its event log, which includes tool results. Pairing those back into turns would
/// drop them or invent a shape. A message list is what the projection produces.
fn as_json(seq: u64, message: &Message) -> serde_json::Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    // `seq` is the log position; it is what `session/fork` takes as `at-seq`.
    // Without it, clients cannot specify a fork point (#106). Sparse on purpose:
    // events with no message (ask, answer, delta) still consume a seq.
    let mut object = serde_json::json!({ "seq": seq, "role": role, "content": message.content });
    // Present only on tool results, tying it to the call it answers.
    if let Some(id) = &message.tool_call_id {
        object["tool-call-id"] = serde_json::json!(id);
    }
    object
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
            let (kind, data) = error_frame(&err);
            let _ = write_frame(&mut *writer.borrow_mut(), kind, &data.to_string());
            return Ok(());
        }
    };

    let mut sink = SseSink {
        writer: Rc::clone(&writer),
        live: true,
    };
    let mut driver = PromptDriver {
        server,
        writer: Rc::clone(&writer),
        session: session.to_string(),
        token,
    };
    if let RunResult::Failed(reason) =
        agent.run_streaming_with_driver(&mut driver, &mut sink, session, &message)
    {
        let (kind, data) = error_frame(&reason);
        let _ = write_frame(&mut *writer.borrow_mut(), kind, &data.to_string());
    }
    Ok(())
}

/// Puts an interceptor's question to the client over the open SSE stream and
/// waits, on this same thread, for the answer to arrive as its own request.
struct PromptDriver<'a> {
    server: &'a Server,
    writer: SharedWriter,
    session: String,
    /// The bearer token, checked here explicitly since this driver serves
    /// the socket while a turn is parked, never passing through `serve_once_authed`.
    token: Option<&'a str>,
}

impl Driver for PromptDriver<'_> {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        let (kind, payload) = prompt_frame(prompt, &self.session);
        if write_frame(&mut *self.writer.borrow_mut(), kind, &payload.to_string()).is_err() {
            // The client is gone; nobody can answer, so take the safe default.
            return prompt.default_answer.clone();
        }
        self.wait_for_answer()
            .unwrap_or_else(|| prompt.default_answer.clone())
    }
}

impl PromptDriver<'_> {
    /// Serve socket until this session's answer arrives or timeout expires.
    /// Other requests refused with 409, not queued: agent is mid-turn and
    /// single-threaded; "busy" clients can retry, hanging ones cannot.
    fn wait_for_answer(&self) -> Option<String> {
        let deadline = Instant::now() + answer_timeout();
        let route = format!("/session/{}/answer", self.session);
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            // Wake every HEARTBEAT so vanished clients are noticed quickly, not at timeout end.
            let slice = remaining.min(HEARTBEAT);
            let Some(mut request) = self.server.recv_timeout(slice).ok().flatten() else {
                if remaining <= slice {
                    // The real deadline, not a tick.
                    return None;
                }
                if write_comment(&mut *self.writer.borrow_mut()).is_err() {
                    // Nobody is listening, so nobody can answer.
                    return None;
                }
                continue;
            };
            let path = request.url().split('?').next().unwrap_or("").to_string();
            if !authorised(&request, self.token, &path) {
                // Drain before replying: unanswered POST leaves connection mid-message.
                let mut discard = String::new();
                let _ = request.as_reader().read_to_string(&mut discard);
                // Refuse and keep waiting: unauthenticated callers cannot answer
                // or consume the wait.
                let _ = respond_json(request, error_reply(401, "missing or invalid bearer token"));
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
                            Reply {
                                status: 200,
                                body: r#"{"accepted":true}"#.to_string(),
                            },
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
/// `None` means no token is configured and everything is allowed — the default
/// bind is loopback, so requiring a secret to talk to your own machine would be
/// friction without a threat. When a token *is* configured it is required
/// everywhere except `/health`, which the supervisor probes and which leaks nothing.
fn authorised(request: &Request, token: Option<&str>, path: &str) -> bool {
    let Some(expected) = token else { return true };
    // `/health` is probed by the supervisor without credentials.
    // The page and bundle are open because they carry no session data and grant
    // nothing. Clients need tokens to read sessions, send messages, or answer
    // prompts — all routes below. See test `an_api_route_still_refuses_without_a_token`.
    if matches!(path, "/health" | "/" | "/app.js") {
        return true;
    }
    request
        .headers()
        .iter()
        .find(|h| {
            h.field
                .as_str()
                .as_str()
                .eq_ignore_ascii_case("authorization")
        })
        .and_then(|h| {
            let value = h.value.as_str();
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
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

/// The web client, compiled into the binary via `include_str!`. No directory
/// needed; resolves at **compile time**. `src/web/dist/` committed rather than
/// built in CI to avoid making Node a build dependency. See
/// `docs/concepts/architecture.md#user-interfaces-separate-clients` (#119).
const WEB_INDEX: &str = include_str!("../../web/dist/index.html");
const WEB_APP_JS: &str = include_str!("../../web/dist/app.js");

/// Serve one embedded asset with its own content type.
fn respond_asset(request: Request, body: &str, content_type: &str) -> std::io::Result<()> {
    let header = Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes())
        .expect("a static content type is valid");
    request.respond(Response::from_string(body).with_header(header))
}

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

/// The SSE frame one turn event projects to: its `event:` kind and `data` JSON.
///
/// Separate from [`SseSink::emit`], which owns the socket and the liveness
/// tracking, so the projection itself can be asserted without one. The client
/// protocol's notifications are the other projection of the same events, and
/// `protocol_events.rs` checks this one loses nothing they keep.
#[must_use]
pub fn sse_frame(event: &Event) -> (&'static str, serde_json::Value) {
    match event {
        Event::TextDelta(text) => ("delta", serde_json::json!({ "text": text })),
        Event::ToolInvoked(call) => (
            "tool",
            // `arguments` used to be dropped here (REST+SSE couldn't render
            // collapsed tool-block lines), but that was a connection-detail bug (#161).
            serde_json::json!({ "id": call.id, "name": call.name, "arguments": call.arguments }),
        ),
        Event::ToolResult(outcome) => (
            "tool-result",
            // `failed` (#162): clients used to infer from prose; now it's explicit.
            serde_json::json!({
                "id": outcome.tool_call_id,
                "content": outcome.content,
                "failed": outcome.failed,
            }),
        ),
        Event::Warning(message) => ("warning", serde_json::json!({ "message": message })),
        Event::Done { text, agentic } => (
            "done",
            serde_json::json!({ "answer": text, "agentic": agentic }),
        ),
    }
}

/// The `prompt` frame: an interceptor's question. Extracted like [`sse_frame`]
/// so it can be tested without a live `Server`.
#[must_use]
pub fn prompt_frame(prompt: &UserPrompt, session: &str) -> (&'static str, serde_json::Value) {
    (
        "prompt",
        serde_json::json!({
            "question": prompt.question,
            "options": prompt.options,
            "default": prompt.default_answer,
            "session": session,
        }),
    )
}

/// The `error` frame. Named so both call sites don't drift apart and the
/// compatibility test can see the shape.
#[must_use]
pub fn error_frame(message: &str) -> (&'static str, serde_json::Value) {
    ("error", serde_json::json!({ "error": message }))
}

impl EventSink for SseSink {
    fn emit(&mut self, event: &Event) -> Flow {
        if !self.live {
            return Flow::Stop;
        }
        let (kind, data) = sse_frame(event);
        if write_frame(&mut *self.writer.borrow_mut(), kind, &data.to_string()).is_err() {
            self.live = false;
            return Flow::Stop;
        }
        Flow::Continue
    }
}

/// Write SSE frame `event: <kind>\ndata: <json>\n\n`, flushed.
fn write_frame(writer: &mut dyn Write, kind: &str, data: &str) -> std::io::Result<()> {
    write!(writer, "event: {kind}\ndata: {data}\n\n")?;
    writer.flush()
}

/// Write SSE comment (`:` line). Not an empty `event:` (UI parser would report
/// unknown kind as error). Protocol has this no-op for keepalives.
fn write_comment(writer: &mut dyn Write) -> std::io::Result<()> {
    write!(writer, ": waiting for an answer\n\n")?;
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
    Reply {
        status,
        body: serde_json::json!({ "error": message }).to_string(),
    }
}

fn json_content_type() -> Header {
    Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap_or_else(|()| {
        Header::from_bytes(&b"X-Content"[..], &b"json"[..]).expect("static header")
    })
}

/// Generate random 16-hex-char session id (stdlib only). Exported so all
/// transports mint the same way; different schemes would collide in the store.
#[must_use]
pub fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Mix timestamp nanos + counter for uniqueness without rand dependency.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::from(d.subsec_nanos()));
    let count = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!(
        "{:08x}{:08x}",
        ts ^ (count << 17),
        count.wrapping_mul(0x9e37_79b9)
    )
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
    use super::{as_json, health, Message, Role};

    #[test]
    fn a_message_serves_its_role_by_name() {
        for (role, expected) in [
            (Role::System, "system"),
            (Role::User, "user"),
            (Role::Assistant, "assistant"),
            (Role::Tool, "tool"),
        ] {
            let json = as_json(
                7,
                &Message {
                    role,
                    content: "x".to_owned(),
                    tool_call_id: None,
                },
            );
            assert_eq!(json["role"], serde_json::json!(expected));
        }
    }

    /// The id is present only on messages that have one, not null on others.
    /// Absent field avoids inviting clients to read it as "sometimes empty".
    #[test]
    fn only_a_tool_result_carries_a_call_id() {
        let plain = as_json(
            1,
            &Message {
                role: Role::User,
                content: "hello".to_owned(),
                tool_call_id: None,
            },
        );
        assert!(plain.get("tool-call-id").is_none(), "{plain}");

        let result = as_json(
            4,
            &Message {
                role: Role::Tool,
                content: "# Jan-Klod".to_owned(),
                tool_call_id: Some("call-1".to_owned()),
            },
        );
        assert_eq!(result["tool-call-id"], serde_json::json!("call-1"));
        assert_eq!(result["content"], serde_json::json!("# Jan-Klod"));
        // Log position travels with message; used by `session/fork` as `at-seq`.
        // Without it, clients cannot specify fork points (#106).
        assert_eq!(result["seq"], serde_json::json!(4));
        assert_eq!(plain["seq"], serde_json::json!(1));
    }

    #[test]
    fn health_reports_ok_with_a_version() {
        let reply = health();
        assert_eq!(reply.status, 200);
        assert!(reply.body.contains("\"status\":\"ok\""), "{}", reply.body);
        assert!(
            reply.body.contains(env!("CARGO_PKG_VERSION")),
            "{}",
            reply.body
        );
    }
}
