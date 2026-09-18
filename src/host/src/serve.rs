//! REST API surface (v0.1.0).
//!
//! Routes: GET /health, GET /sessions, POST /sessions, GET /session/:id, POST
//! /session/:id/message (SSE or JSON), answer, fork, GET /contributions and
//! POST /contributions/invoke.
//!
//! # Who runs on which thread (#225)
//!
//! One thread accepts. The session lives on another and is reached by job
//! (`session_thread`), because `AgentSession` is `!Send` and cannot be
//! handed to whoever happens to have accepted a request.
//!
//! * Anything that needs no session — the assets, `/health`, and **the
//!   answer route** — is served on the accepting thread.
//! * Anything that does is handed to a short-lived worker, which blocks on
//!   the session's queue. Blocking there is correct: the session is busy.
//!   Blocking the *accepting* thread would not be, which is why these are
//!   not served inline.
//! * A streaming turn is queued and not waited for. Its writer moves to the
//!   session's thread (`Request::into_writer` is `Send`) and the turn
//!   streams from there.
//!
//! # Mid-turn confirmations
//!
//! An interceptor blocks in `Driver::ask`, which registers a one-shot in
//! [`crate::pending`] and waits on it. The answer arrives as an ordinary
//! request, is completed by the accepting thread, and never becomes a job —
//! queued behind the parked turn it would wait for the turn waiting for it.
//!
//! This replaces a driver that **served the socket itself** while parked and
//! refused every other caller with `409`. `409` now means only what it says:
//! nothing is pending for that session. Unanswered prompts still time out at
//! [`answer_timeout`], taking the prompt's default.

use std::cell::RefCell;
use std::io::Write;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tiny_http::{Header, Method, Request, Response, Server};

use jan_klod_core::conductor::{Event, EventSink, Flow, RunResult};
use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::session::{
    answer_timeout, fork, new_session_id, session_payload, sessions_payload, Forked,
};
use jan_klod_core::AgentSession;

use crate::pending::Pending;
use crate::session_thread::{self, Jobs};

/// How often the wait pokes the event stream while parked. A vanished client
/// (post-write) looks fine until FIN is processed; each tick writes an SSE
/// comment: dead peers surface as write errors within one interval, and live
/// streams get keepalive bytes. Comments (`:` lines) are protocol no-ops.
const HEARTBEAT: Duration = Duration::from_secs(5);

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
/// Returns the underlying I/O error if the request cannot be received.
pub fn serve_once(server: &Server, agent: &mut AgentSession) -> std::io::Result<()> {
    serve_requests(server, agent, None, 1)
}

/// Serve one request, requiring `Bearer <token>` when `token` is `Some`.
///
/// # Errors
/// Returns the underlying I/O error if the request cannot be received.
pub fn serve_once_authed(
    server: &Server,
    agent: &mut AgentSession,
    token: Option<&str>,
) -> std::io::Result<()> {
    serve_requests(server, agent, token, 1)
}

/// Serve exactly `requests` requests, then return.
///
/// The bounded form exists for tests, and it is not a second code path: the
/// production loop below is this with no bound. A confirmation needs **two**
/// — the turn and the answer — which is the shape that stopped being
/// expressible in one when the parked driver gave up the socket (#225).
///
/// # Errors
/// Returns the underlying I/O error if a request cannot be received.
pub fn serve_requests(
    server: &Server,
    agent: &mut AgentSession,
    token: Option<&str>,
    requests: usize,
) -> std::io::Result<()> {
    // Owned once, shared with every worker: a job cannot borrow from this
    // frame, and a second registry would mean an answer that reaches nobody.
    let pending = Arc::new(Pending::new());
    let token = token.map(str::to_owned);
    let (jobs, queued) = session_thread::queue::<AgentSession>();

    std::thread::scope(|scope| {
        let accepting = {
            let pending = Arc::clone(&pending);
            let token = token.clone();
            scope.spawn(move || -> std::io::Result<()> {
                for _ in 0..requests {
                    let request = server.recv()?;
                    dispatch(scope, request, &jobs, &pending, token.as_deref());
                }
                // Dropping the last handle is what ends the loop below.
                Ok(())
            })
        };
        // This thread owns the session for as long as anything can ask.
        session_thread::serve(agent, &queued);
        accepting
            .join()
            .unwrap_or_else(|_| Err(std::io::Error::other("the accept thread panicked")))
    })
}

/// Route one request: inline when it needs no session, on a worker when it
/// does, queued when it streams.
fn dispatch<'scope>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    request: Request,
    jobs: &Jobs<AgentSession>,
    pending: &Arc<Pending>,
    token: Option<&str>,
) {
    let method = request.method().clone();
    let url = request.url().to_string();
    // Strip query string for routing.
    let path = url.split('?').next().unwrap_or(&url).to_string();

    // Auth first. `/health` stays open: no session data, supervisor probes unauthed.
    if !authorised(&request, token, &path) {
        let _ = respond_json(request, error_reply(401, "missing or invalid bearer token"));
        return;
    }

    // Anything needing no session is answered here and now.
    let Some(mut request) = without_session(request, &method, &path, pending) else {
        return;
    };

    // ── Streaming: queued, never waited for ─────────────────────────────
    //
    // Waiting here would hold the accepting thread for the whole turn, and
    // the answer to its own confirmation would never be accepted.
    if method == Method::Post {
        if let Some(session) = strip_prefix(&path, "/session/")
            .and_then(|rest| rest.strip_suffix("/message"))
            .filter(|id| !id.contains('/'))
            .map(str::to_owned)
        {
            let wants_sse = accepts_event_stream(&request);
            let mut body = String::new();
            if request.as_reader().read_to_string(&mut body).is_err() {
                let _ = respond_json(request, error_reply(400, "unreadable body"));
                return;
            }
            if wants_sse {
                let writer = request.into_writer();
                let pending = Arc::clone(pending);
                let _ = jobs.send(move |agent: &mut AgentSession| {
                    run_turn_streaming(agent, writer, &session, &body, &pending);
                });
            } else {
                on_worker(scope, jobs, request, move |agent| {
                    handle_message(agent, &session, &body)
                });
            }
            return;
        }
    }

    // ── Everything else needs the session, on a worker ──────────────────
    if method == Method::Get && path == "/sessions" {
        on_worker(scope, jobs, request, |agent: &mut AgentSession| {
            handle_list_sessions(agent)
        });
        return;
    }
    if method == Method::Get && path == "/contributions" {
        on_worker(scope, jobs, request, handle_contributions);
        return;
    }
    if method == Method::Post && path == "/contributions/invoke" {
        let mut body = String::new();
        if request.as_reader().read_to_string(&mut body).is_err() {
            let _ = respond_json(request, error_reply(400, "unreadable body"));
            return;
        }
        on_worker(scope, jobs, request, move |agent| {
            handle_invoke(agent, &body)
        });
        return;
    }
    if method == Method::Get {
        if let Some(id) = strip_prefix(&path, "/session/")
            .filter(|id| !id.contains('/'))
            .map(str::to_owned)
        {
            on_worker(scope, jobs, request, move |agent| {
                handle_get_session(agent, &id)
            });
            return;
        }
    }
    if method == Method::Post {
        if let Some(id) = strip_prefix(&path, "/session/")
            .and_then(|rest| rest.strip_suffix("/fork"))
            .filter(|id| !id.contains('/'))
            .map(str::to_owned)
        {
            let mut body = String::new();
            if request.as_reader().read_to_string(&mut body).is_err() {
                let _ = respond_json(request, error_reply(400, "unreadable body"));
                return;
            }
            on_worker(scope, jobs, request, move |agent| {
                handle_fork_session(agent, &id, &body)
            });
            return;
        }
    }

    let _ = respond_json(request, error_reply(404, "not found"));
}

/// Serve the routes that need no session, on the accepting thread.
///
/// `Some(request)` hands it back unanswered for the session-bound routes
/// below. The answer route is in **this** group deliberately: as a job it
/// would queue behind the very turn that is parked waiting for it.
fn without_session(
    mut request: Request,
    method: &Method,
    path: &str,
    pending: &Pending,
) -> Option<Request> {
    if *method == Method::Get {
        match path {
            "/" => {
                let _ = respond_asset(request, WEB_INDEX, "text/html; charset=utf-8");
                return None;
            }
            "/app.js" => {
                let _ = respond_asset(request, WEB_APP_JS, "text/javascript; charset=utf-8");
                return None;
            }
            "/health" => {
                let _ = respond_json(request, health());
                return None;
            }
            _ => return Some(request),
        }
    }
    if *method != Method::Post {
        return Some(request);
    }
    if path == "/sessions" {
        let _ = respond_json(request, handle_create_session());
        return None;
    }
    let answer_for = strip_prefix(path, "/session/").and_then(|rest| {
        rest.strip_suffix("/answer")
            .filter(|id| !id.contains('/'))
            .map(str::to_owned)
    });
    if let Some(session) = answer_for {
        let reply = request_answer(&mut request, &session, pending);
        let _ = respond_json(request, reply);
        return None;
    }
    Some(request)
}

/// Run `handler` on the session's thread and answer this request with what
/// it returns, on a thread of this request's own.
///
/// The worker exists so the accepting thread keeps accepting: a request
/// that needs a busy session waits, and nothing else has to wait with it.
fn on_worker<'scope, F>(
    scope: &'scope std::thread::Scope<'scope, '_>,
    jobs: &Jobs<AgentSession>,
    request: Request,
    handler: F,
) where
    F: FnOnce(&mut AgentSession) -> Reply + Send + 'static,
{
    let jobs = jobs.clone();
    scope.spawn(move || {
        let reply = jobs
            .run(handler)
            .unwrap_or_else(|why| error_reply(503, &format!("the session is unavailable: {why}")));
        let _ = respond_json(request, reply);
    });
}

/// `POST /session/:id/answer` — hand the answer to the turn parked on it.
///
/// Consumes the request's body and answers it here, on the accepting
/// thread. Returns the request so the caller can respond with the reply
/// this produced.
fn request_answer(request: &mut Request, session: &str, pending: &Pending) -> Reply {
    let mut body = String::new();
    if request.as_reader().read_to_string(&mut body).is_err() {
        return error_reply(400, "unreadable body");
    }
    match parse_answer_body(&body) {
        Err(err) => error_reply(400, &err),
        Ok(answer) => {
            if pending.answer(session, answer) {
                Reply {
                    status: 200,
                    body: r#"{"accepted":true}"#.to_string(),
                }
            } else {
                // Still a true statement, and the only one `409` makes now:
                // nothing is parked for this session. What it no longer
                // means is "someone else is confirming".
                error_reply(409, "no confirmation is pending")
            }
        }
    }
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

/// `GET /contributions` — what the loaded extensions offer a client.
///
/// A route rather than a frame, and **no SSE frame at all**, which is a
/// difference from the stdio transport worth stating. There, the set is
/// pushed at the handshake and again when it changes, because the connection
/// is open the whole time. Here there is no handshake to push at, and a
/// stream exists only while a turn runs — so the set is *read*, and the one
/// thing that can change it mid-session says so in its own answer (see
/// [`handle_invoke`]). Inventing a frame nothing could emit outside a turn
/// would be a projection of nothing.
fn handle_contributions(agent: &mut AgentSession) -> Reply {
    Reply {
        status: 200,
        body: contributions_payload(agent).to_string(),
    }
}

/// `POST /contributions/invoke` — run one, as `{"extension":…,"name":…}`.
///
/// The answer carries `contributions-changed`, which is how a client learns
/// to re-read the set: it asked, so it is listening, and that is the only
/// moment the set can move while a session is up.
fn handle_invoke(agent: &mut AgentSession, body: &str) -> Reply {
    let value: serde_json::Value = match serde_json::from_str(body) {
        Ok(value) => value,
        Err(err) => return error_reply(400, &format!("invalid JSON body: {err}")),
    };
    let (Some(extension), Some(name)) = (
        value.get("extension").and_then(serde_json::Value::as_str),
        value.get("name").and_then(serde_json::Value::as_str),
    ) else {
        return error_reply(400, "both `extension` and `name` are required");
    };
    let arguments: Vec<jan_klod_core::contributions::ArgumentValue> = value
        .get("arguments")
        .and_then(serde_json::Value::as_array)
        .map(|given| {
            given
                .iter()
                .filter_map(|argument| {
                    Some(jan_klod_core::contributions::ArgumentValue {
                        name: argument.get("name")?.as_str()?.to_owned(),
                        value: argument.get("value")?.as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    match agent.invoke_contribution(extension, name, &arguments) {
        Ok(outcome) => Reply {
            status: 200,
            body: serde_json::json!({
                "text": outcome.text,
                "contributions-changed": outcome.contributions_changed,
            })
            .to_string(),
        },
        // A name nobody contributes is the caller's mistake; the other two are
        // the extension's, and the status codes say which.
        Err(error @ jan_klod_core::contributions::InvokeError::Unknown) => {
            error_reply(404, &error.to_string())
        }
        Err(error @ jan_klod_core::contributions::InvokeError::InvalidArguments) => {
            error_reply(400, &error.to_string())
        }
        Err(error @ jan_klod_core::contributions::InvokeError::Failed(_)) => {
            error_reply(500, &error.to_string())
        }
    }
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

/// `GET /session/:id` — return transcript + metadata.
fn handle_get_session(agent: &AgentSession, id: &str) -> Reply {
    Reply {
        status: 200,
        body: session_payload(agent, id).to_string(),
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

/// `POST /session/:id/message` (SSE variant) — stream a turn, on the
/// session's own thread.
///
/// The writer arrives here rather than the request: `Request::into_writer`
/// hands back a `Box<dyn Write + Send>`, which is the one thing about this
/// surface that may cross to the session's thread, and the reason a turn
/// can stream from where the session lives.
fn run_turn_streaming(
    agent: &mut AgentSession,
    writer: Box<dyn Write + Send>,
    session: &str,
    body: &str,
    pending: &Pending,
) {
    let writer: SharedWriter = Rc::new(RefCell::new(writer));
    if writer
        .borrow_mut()
        .write_all(
            b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
          Cache-Control: no-cache\r\nConnection: close\r\n\r\n",
        )
        .is_err()
    {
        return;
    }

    let message = match parse_message_body(body) {
        Ok(m) => m,
        Err(err) => {
            let (kind, data) = error_frame(&err);
            let _ = write_frame(&mut *writer.borrow_mut(), kind, &data.to_string());
            return;
        }
    };

    let mut sink = SseSink {
        writer: Rc::clone(&writer),
        live: true,
    };
    let mut driver = PromptDriver {
        pending,
        writer: Rc::clone(&writer),
        session: session.to_string(),
    };
    if let RunResult::Failed(reason) =
        agent.run_streaming_with_driver(&mut driver, &mut sink, session, &message)
    {
        let (kind, data) = error_frame(&reason);
        let _ = write_frame(&mut *writer.borrow_mut(), kind, &data.to_string());
    }
}

/// Puts an interceptor's question to the client over the open SSE stream
/// and waits for the answer to arrive as its own request.
///
/// It no longer reads the socket to get one. The answer is delivered by
/// whichever thread accepted it, through [`Pending`] — so a parked turn
/// stops being the server, and the `409` every other caller used to get
/// stops being a thing that happens.
struct PromptDriver<'a> {
    pending: &'a Pending,
    writer: SharedWriter,
    session: String,
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
    /// Wait for this session's answer, writing a heartbeat between slices.
    ///
    /// The deadline is here rather than in [`Pending`] because the
    /// heartbeat is a *write* to this stream: a vanished client looks fine
    /// until a write fails, so waking every `HEARTBEAT` is how one is
    /// noticed before the deadline rather than at it. Unchanged from the
    /// version that owned the socket — only what it waits on has changed.
    fn wait_for_answer(&self) -> Option<String> {
        let Some(parked) = self.pending.park(&self.session) else {
            // Something is already parked for this session. Two questions
            // and one answer route cannot be told apart, so this one takes
            // its default rather than risk being given the other's answer.
            return None;
        };
        let deadline = Instant::now() + answer_timeout();
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let slice = remaining.min(HEARTBEAT);
            if let Some(answer) = parked.wait(slice) {
                return Some(answer);
            }
            if remaining <= slice {
                // The real deadline, not a tick.
                return None;
            }
            if write_comment(&mut *self.writer.borrow_mut()).is_err() {
                // Nobody is listening, so nobody can answer.
                return None;
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

/// What the extensions contribute, as this surface reports it.
///
/// The same information the client protocol's `surface/contributions` carries,
/// shaped for a reader that asked: one object per extension, in load order.
/// Field names match the protocol's so a client written against the schema
/// reads either without a second mapping.
fn contributions_payload(agent: &mut AgentSession) -> serde_json::Value {
    let extensions: Vec<serde_json::Value> = agent
        .contributions()
        .into_iter()
        .map(|set| {
            serde_json::json!({
                "extension": set.extension,
                "commands": set
                    .commands
                    .into_iter()
                    .map(|command| serde_json::json!({
                        "name": command.name,
                        "title": command.title,
                        "description": command.description,
                        "arguments": command
                            .arguments
                            .into_iter()
                            .map(|argument| serde_json::json!({
                                "name": argument.name,
                                "description": argument.description,
                                "required": argument.required,
                            }))
                            .collect::<Vec<_>>(),
                    }))
                    .collect::<Vec<_>>(),
                "status-items": set
                    .status_items
                    .into_iter()
                    .map(|item| serde_json::json!({
                        "name": item.name,
                        "text": item.text,
                        "detail": item.detail,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    serde_json::json!({ "extensions": extensions })
}

#[cfg(test)]
mod tests {
    use super::health;

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
