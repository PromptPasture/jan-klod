//! Host-side inbound HTTP surface — resource-model REST API.
//!
//! Routes (v0.1.0):
//!   GET  /health                   liveness probe
//!   GET  /sessions                 list session ids + previews
//!   POST /sessions                 create session → `{"id":"<id>"}`
//!   GET  /session/:id              message list, projected from the event log
//!   POST /session/:id/message      send a message; SSE or JSON response
//!   POST /session/:id/answer       answer a pending confirmation
//!   POST /session/:id/fork         new session from a prefix of this one
//!
//! Synchronous/blocking (`tiny_http`) — one request is served at a time on the
//! thread that owns the `!Send` `AgentSession`.
//!
//! ## Answering a mid-turn confirmation without threads
//!
//! An interceptor (e.g. the permission gate) can stop a turn to ask the user
//! something; the loop is synchronous, so it *blocks* inside `Driver::ask`, and
//! the answer must arrive on a different, concurrent request.
//!
//! Rather than make `AgentSession` `Send` and use worker threads, the waiting
//! driver **serves the socket itself**: it emits a `prompt` SSE frame, then calls
//! `Server::recv_timeout` in a loop until a matching `POST /session/:id/answer`
//! arrives, replying `409` to anything else. Concurrency stays at one request at
//! a time. An unanswered prompt times out at [`DEFAULT_ANSWER_TIMEOUT`] and takes
//! the prompt's own default (a denial, for the permission gate).

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

/// Overrides [`DEFAULT_ANSWER_TIMEOUT`], in seconds.
///
/// Three minutes is right for a person and wrong for a test: a test whose answer
/// goes astray would otherwise wait out the whole timeout and still *pass*
/// (default answer is a denial, assertions still hold), just slow. Read per wait
/// rather than cached, so a test can set it per process.
const TIMEOUT_ENV: &str = "JK_ANSWER_TIMEOUT_SECS";

/// How often the wait pokes the event stream while parked.
///
/// A client that disappears after the first write leaves a socket that looks
/// fine until the FIN is processed, which can take the whole timeout. Each tick
/// writes an SSE comment: a dead peer surfaces as a write error within one
/// interval, and a live stream gets bytes that keep a reverse proxy from closing
/// it as idle. Comments (`:`-prefixed lines) are the protocol's own no-op.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// The configured confirmation timeout.
///
/// `pub` so the stdio transport parks for the same length of time this one
/// does: an answer window that depended on which transport a client happened to
/// use would be a surprise nobody could have read anywhere.
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

    // Auth first, before any route can act. `/health` stays open: it carries no
    // session data and the blue/green supervisor probes it without credentials.
    if !authorised(&request, token, path) {
        return respond_json(request, error_reply(401, "missing or invalid bearer token"));
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
/// Here rather than in [`crate::rpc`] because this is where it was written, and
/// one shape is the point: a client that lists sessions over stdio and over
/// REST must not have to render two.
#[must_use]
pub fn sessions_payload(agent: &AgentSession) -> serde_json::Value {
    let sessions: Vec<serde_json::Value> = agent
        .list_sessions()
        .into_iter()
        .map(|id| {
            // The first thing the user said, which is what makes a session
            // recognisable in a picker. Taken from the projection rather than
            // from row 1 of the log, because row 1 need not be a user message
            // in a session whose first turn was steered or interrupted.
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

/// `POST /session/:id/fork` — start a new session from a prefix of this one.
///
/// The child's id is generated here, as `POST /sessions` generates one: a
/// client naming it could collide with a live session, and `fork_events`
/// refuses to write into a log that already exists, so the failure would be a
/// confusing 500 rather than an id the client cannot pick wrongly.
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
/// are not the same failure, and every transport has to say so in its own
/// vocabulary: the caller asked for something that is not there (`404`, or
/// JSON-RPC `invalid params`) or the store broke (`500`, or `internal error`).
/// Collapsing them would make a mistyped `at-seq` look like a broken database.
pub enum Forked {
    /// The child session, as `{"id": …, "copied": …}`.
    Created(serde_json::Value),
    /// Nothing was copied, so no fork was made.
    Empty(String),
    /// The store refused.
    Failed(String),
}

/// Fork `id` at `at_seq` into a freshly generated child session.
///
/// The child's id is generated here, as `POST /sessions` generates one: a
/// client naming it could collide with a live session, and `fork_events`
/// refuses to write into a log that already exists, so the failure would be a
/// confusing internal error rather than an id the client cannot pick wrongly.
#[must_use]
pub fn fork(agent: &AgentSession, id: &str, at_seq: u64) -> Forked {
    let child = new_session_id();
    match agent.fork_session(id, at_seq, &child) {
        // Copying nothing means the fork would start empty, which creating a
        // session already does better. Almost always a wrong `at-seq` or a
        // wrong session id, so it is reported rather than returning a session
        // that silently is not a fork of anything.
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

/// One session's message list, as any transport reports it. Shared for the same
/// reason [`sessions_payload`] is.
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
/// `messages`, not the `turns` this used to return. A turn was a
/// `{user, answer}` pair because that is what the transcript row held; the
/// session is now projected from its event log, which has tool results in it
/// too, and pairing those back into turns would have to either drop them or
/// invent a shape for them. A message list is what the projection produces and
/// what a client can render without guessing.
fn as_json(seq: u64, message: &Message) -> serde_json::Value {
    let role = match message.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };
    // `seq` is the log position this message was projected from, and it is what
    // `session/fork` takes as `at-seq`. Without it a client holds the messages
    // and not their places, so "fork from here" cannot be spelled at all
    // ([#106](https://github.com/PromptPasture/jan-klod/issues/106)).
    //
    // Sparse on purpose: events that project to no message (an ask, an answer,
    // a text delta) still consume a seq, so this is a position in the log and
    // not an index into `messages`.
    let mut object = serde_json::json!({ "seq": seq, "role": role, "content": message.content });
    // Present only where it means something — on a tool result, tying it to the
    // call it answers.
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
    /// The bearer token, if one is configured. Checked here explicitly, since
    /// this driver serves the socket itself while a turn is parked and never
    /// passes through `serve_once_authed`.
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
    /// Serve the socket until this session's answer arrives, or the wait expires.
    ///
    /// Anything else received meanwhile is refused with `409` rather than queued:
    /// the agent is mid-turn and single-threaded, and a client that is told
    /// "busy" can retry, while one left hanging cannot.
    fn wait_for_answer(&self) -> Option<String> {
        let deadline = Instant::now() + answer_timeout();
        let route = format!("/session/{}/answer", self.session);
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            // Wake at least every HEARTBEAT so a vanished client is noticed in
            // seconds rather than at the far end of the timeout.
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
                // Drain before replying: the body is still in the socket, and
                // answering a POST without consuming it leaves the connection
                // mid-message — the client then waits for bytes that never come.
                let mut discard = String::new();
                let _ = request.as_reader().read_to_string(&mut discard);
                // Refuse and keep waiting: an unauthenticated caller must not be
                // able to answer a permission prompt, nor to cancel one by
                // consuming the wait.
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
/// everywhere except `/health`, which the supervisor probes and which leaks
/// nothing.
fn authorised(request: &Request, token: Option<&str>, path: &str) -> bool {
    let Some(expected) = token else { return true };
    if path == "/health" {
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

/// The SSE frame one turn event projects to: its `event:` kind and `data` JSON.
///
/// Separate from [`SseSink::emit`], which owns the socket and the liveness
/// tracking, so the projection itself can be asserted without one. The client
/// protocol's notifications are the other projection of the same events, and
/// `core/tests/protocol_events.rs` checks this one loses nothing they keep.
#[must_use]
pub fn sse_frame(event: &Event) -> (&'static str, serde_json::Value) {
    match event {
        Event::TextDelta(text) => ("delta", serde_json::json!({ "text": text })),
        Event::ToolInvoked(call) => (
            "tool",
            serde_json::json!({ "id": call.id, "name": call.name }),
        ),
        Event::ToolResult(outcome) => (
            "tool-result",
            serde_json::json!({ "id": outcome.tool_call_id, "content": outcome.content }),
        ),
        Event::Warning(message) => ("warning", serde_json::json!({ "message": message })),
        Event::Done { text, agentic } => (
            "done",
            serde_json::json!({ "answer": text, "agentic": agentic }),
        ),
    }
}

/// The `prompt` frame: an interceptor's question, put to the client.
///
/// Extracted alongside [`sse_frame`] for the same reason — [`PromptDriver::ask`]
/// needs a live `Server` to run, this needs nothing.
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

/// The `error` frame: an unservable request, or a turn that failed.
///
/// Both call sites went through the same inline `json!`; naming it keeps them
/// from drifting apart and lets the compatibility test see the shape.
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

/// Write one SSE frame: `event: <kind>\ndata: <json>\n\n`, flushed.
fn write_frame(writer: &mut dyn Write, kind: &str, data: &str) -> std::io::Result<()> {
    write!(writer, "event: {kind}\ndata: {data}\n\n")?;
    writer.flush()
}

/// Write an SSE comment: a `:`-prefixed line a conformant client ignores.
///
/// Not an empty `event:` frame — the UI client's parser reports an unknown event
/// kind, so a keepalive would surface to the user as an error. The protocol has a
/// no-op for exactly this and it costs three bytes.
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

/// Generate a random hex session id (16 hex chars, using stdlib only).
///
/// `pub` so every transport mints them the same way — two schemes would
/// eventually collide, and the collision would land in the store.
#[must_use]
pub fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Mix timestamp nanos with a counter to get unique ids without a rand dep.
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

    /// The id is present only on a message that has one. A `"tool-call-id":
    /// null` on every user message would invite a client to read it as a field
    /// that is sometimes empty rather than sometimes absent.
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
        // The log position travels with the message: it is what `session/fork`
        // takes as `at-seq`, and without it a client cannot name a fork point
        // ([#106](https://github.com/PromptPasture/jan-klod/issues/106)).
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
