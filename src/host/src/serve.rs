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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

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

/// A ready HTTP reply: status code + JSON body.
pub struct Reply {
    /// HTTP status code.
    pub status: u16,
    /// JSON body.
    pub body: String,
}

// ─── Route dispatch ──────────────────────────────────────────────────────────

/// A bound port, ready to serve.
///
/// Binding is separate from serving because a test needs the port before
/// anything is served on it — it has to tell a client where to knock. The
/// production path binds and serves in two statements for the same reason:
/// the address is reported to the operator before the loop starts.
pub struct Surface {
    listener: std::net::TcpListener,
}

/// What every handler shares.
///
/// `Jobs` is in here rather than cloned per route because `axum` wants
/// state it can hand to each request by reference; that is what made
/// `Jobs` `Sync` (`session_thread`).
#[derive(Clone)]
struct App {
    jobs: Arc<Jobs<AgentSession>>,
    pending: Arc<Pending>,
    token: Option<Arc<str>>,
    /// Responses completed so far, against `limit`.
    served: Arc<AtomicUsize>,
    /// How many responses to complete before shutting down. `usize::MAX`
    /// is the production loop, which is the same code with no bound.
    limit: usize,
    /// Raised when the surface should stop.
    stop: Arc<tokio::sync::Notify>,
}

impl App {
    /// Run `handler` on the session's thread and answer with what it
    /// returns.
    ///
    /// `spawn_blocking`, not a direct call: the session's queue blocks, and
    /// blocking a runtime worker for the length of a turn would stall every
    /// other request on that worker. This is the one thing about the port
    /// that is not visible in a test until the surface is under load.
    async fn on_session<F>(&self, handler: F) -> Response
    where
        F: FnOnce(&mut AgentSession) -> Reply + Send + 'static,
    {
        let jobs = Arc::clone(&self.jobs);
        let reply = tokio::task::spawn_blocking(move || jobs.run(handler)).await;
        match reply {
            Ok(Ok(reply)) => reply.into_response(),
            Ok(Err(why)) => {
                error_reply(503, &format!("the session is unavailable: {why}")).into_response()
            }
            Err(_) => error_reply(500, "the request was cancelled").into_response(),
        }
    }

    /// Count a completed response and stop once the bound is reached.
    fn served_one(&self) {
        if self.limit == usize::MAX {
            return;
        }
        if self.served.fetch_add(1, Ordering::Relaxed) + 1 >= self.limit {
            self.stop.notify_waiters();
        }
    }
}

impl Surface {
    /// Bind `addr` without serving anything yet.
    ///
    /// # Errors
    /// Whatever binding the address failed with.
    pub fn bind(addr: &str) -> std::io::Result<Self> {
        Ok(Self {
            listener: std::net::TcpListener::bind(addr)?,
        })
    }

    /// The port it is listening on.
    ///
    /// # Panics
    /// If the listener has no local address, which cannot happen for a
    /// bound socket.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.listener.local_addr().expect("a bound listener").port()
    }

    /// Serve one request, then return.
    ///
    /// # Errors
    /// Whatever the server failed with.
    pub fn serve_once(&self, agent: &mut AgentSession) -> std::io::Result<()> {
        self.run(agent, None, 1, None::<fn(u16)>)
    }

    /// Serve one request, requiring `Bearer <token>` when one is set.
    ///
    /// # Errors
    /// Whatever the server failed with.
    pub fn serve_once_authed(
        &self,
        agent: &mut AgentSession,
        token: Option<&str>,
    ) -> std::io::Result<()> {
        self.run(agent, token, 1, None::<fn(u16)>)
    }

    /// Serve while `client` runs, then stop, returning what it returned.
    ///
    /// # Panics
    /// Propagates a panic from `client` rather than hanging on it.
    pub fn serve_while<F, R>(&self, agent: &mut AgentSession, token: Option<&str>, client: F) -> R
    where
        F: FnOnce(u16) -> R + Send,
        R: Send,
    {
        let answer = std::sync::Mutex::new(None);
        let _ = self.run(
            agent,
            token,
            usize::MAX,
            Some(|port| {
                let out = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| client(port)));
                *answer
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(out);
            }),
        );
        match answer
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            Some(Ok(out)) => out,
            Some(Err(panic)) => std::panic::resume_unwind(panic),
            None => unreachable!("the client closure always runs"),
        }
    }

    /// Serve until killed.
    ///
    /// # Errors
    /// Whatever the server failed with.
    pub fn serve_forever(
        &self,
        agent: &mut AgentSession,
        token: Option<&str>,
    ) -> std::io::Result<()> {
        // No closure: nothing should ever ask this one to stop, and a
        // thread parked to represent "forever" would be a thread nothing
        // can wake if the server fails.
        self.run(agent, token, usize::MAX, None::<fn(u16)>)
    }

    /// The one loop all four entry points are.
    ///
    /// The session stays on **this** thread — it cannot leave — so the
    /// runtime runs on another, and `alongside` is whatever the caller
    /// wants doing while it serves.
    fn run<F>(
        &self,
        agent: &mut AgentSession,
        token: Option<&str>,
        limit: usize,
        alongside: Option<F>,
    ) -> std::io::Result<()>
    where
        F: FnOnce(u16) + Send,
    {
        let (jobs, queued) = session_thread::queue::<AgentSession>();
        let app = App {
            jobs: Arc::new(jobs),
            pending: Arc::new(Pending::new()),
            token: token.map(Arc::from),
            served: Arc::new(AtomicUsize::new(0)),
            limit,
            stop: Arc::new(tokio::sync::Notify::new()),
        };
        let listener = self.listener.try_clone()?;
        listener.set_nonblocking(true)?;
        let port = self.port();
        let stop = Arc::clone(&app.stop);

        let outcome = std::thread::scope(|scope| {
            let serving = {
                let app = app.clone();
                let stopping = Arc::clone(&app.stop);
                scope.spawn(move || -> std::io::Result<()> {
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .enable_all()
                        .build()?;
                    runtime.block_on(async move {
                        let listener = tokio::net::TcpListener::from_std(listener)?;
                        axum::serve(listener, router(app))
                            .with_graceful_shutdown(async move { stopping.notified().await })
                            .await
                    })
                })
            };
            if let Some(alongside) = alongside {
                scope.spawn(move || {
                    alongside(port);
                    // Done means stop. Without a closure — the production
                    // loop — nothing here ever asks it to.
                    stop.notify_waiters();
                });
            }
            // Everything the surface holds is dropped when the runtime
            // thread ends, which is what closes the queue below.
            drop(app);
            session_thread::serve(agent, &queued);
            serving.join()
        });
        outcome.unwrap_or_else(|_| Err(std::io::Error::other("the serving thread panicked")))
    }
}

/// Every route, with `App` behind them.
fn router(app: App) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/app.js", get(app_js))
        .route("/health", get(health_route))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/contributions", get(contributions))
        .route("/contributions/invoke", post(invoke))
        .route("/session/{id}", get(get_session))
        .route("/session/{id}/message", post(message))
        .route("/session/{id}/answer", post(answer))
        .route("/session/{id}/fork", post(fork_session))
        .fallback(not_found)
        .layer(axum::middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

// ─── The routes, as `axum` sees them ─────────────────────────────────────────

/// Bearer-token check and the response counter, in one layer.
///
/// One place, as the module promises: every route passes through here, so
/// a route added without thinking about auth is still covered.
async fn guard(
    State(app): State<App>,
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if !authorised(request.headers(), app.token.as_deref(), &path) {
        app.served_one();
        return error_reply(401, "missing or invalid bearer token").into_response();
    }
    let response = next.run(request).await;
    app.served_one();
    response
}

async fn index() -> Response {
    asset(WEB_INDEX, "text/html; charset=utf-8")
}

async fn app_js() -> Response {
    asset(WEB_APP_JS, "text/javascript; charset=utf-8")
}

async fn health_route() -> Response {
    health().into_response()
}

async fn not_found() -> Response {
    error_reply(404, "not found").into_response()
}

async fn create_session() -> Response {
    handle_create_session().into_response()
}

async fn list_sessions(State(app): State<App>) -> Response {
    app.on_session(|agent: &mut AgentSession| handle_list_sessions(agent))
        .await
}

async fn contributions(State(app): State<App>) -> Response {
    app.on_session(handle_contributions).await
}

async fn invoke(State(app): State<App>, body: String) -> Response {
    app.on_session(move |agent| handle_invoke(agent, &body))
        .await
}

async fn get_session(State(app): State<App>, Path(id): Path<String>) -> Response {
    app.on_session(move |agent| handle_get_session(agent, &id))
        .await
}

async fn fork_session(State(app): State<App>, Path(id): Path<String>, body: String) -> Response {
    app.on_session(move |agent| handle_fork_session(agent, &id, &body))
        .await
}

/// `POST /session/{id}/answer` — hand the answer to the turn parked on it.
///
/// Answered here rather than on the session's thread. As a job it would
/// queue behind the very turn that is parked waiting for it (#225), and no
/// transport changes that.
async fn answer(State(app): State<App>, Path(id): Path<String>, body: String) -> Response {
    match parse_answer_body(&body) {
        Err(err) => error_reply(400, &err).into_response(),
        Ok(answer) => {
            if app.pending.answer(&id, answer) {
                Reply {
                    status: 200,
                    body: r#"{"accepted":true}"#.to_string(),
                }
                .into_response()
            } else {
                // Still true, and now the only thing it says: nothing is
                // parked for this session.
                error_reply(409, "no confirmation is pending").into_response()
            }
        }
    }
}

/// `POST /session/{id}/message` — JSON, or a stream when asked for one.
async fn message(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    if !accepts_event_stream(&headers) {
        return app
            .on_session(move |agent| handle_message(agent, &id, &body))
            .await;
    }
    // The turn runs on the session's thread and sends frames here. Queued,
    // never awaited: a turn can park for as long as a person takes, and
    // the surface has to keep serving meanwhile.
    let (frames, stream) = unbounded_channel::<Frame>();
    let pending = Arc::clone(&app.pending);
    let queued = app.jobs.send(move |agent: &mut AgentSession| {
        run_turn_streaming(agent, &frames, &id, &body, &pending);
    });
    if queued.is_err() {
        return error_reply(503, "the session is unavailable").into_response();
    }
    Sse::new(Frames(stream)).into_response()
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

/// One SSE frame on its way to a client.
///
/// A message rather than bytes on a socket: the response is a stream now,
/// and the turn that produces frames runs on a different thread from the
/// one writing them. A closed receiver — the client went away — is how a
/// vanished client is noticed, which is the job the heartbeat write used
/// to do.
enum Frame {
    /// `event: <kind>` with a JSON `data:` line.
    Named {
        /// The `event:` name.
        kind: &'static str,
        /// Its JSON payload.
        data: String,
    },
    /// A `:` comment, which the protocol treats as a no-op keepalive.
    Comment,
}

/// The frames of one turn, as a stream `axum` can serve.
///
/// Hand-written rather than `tokio-stream`'s wrapper, which would be a
/// package for ten lines. `futures_core` is already in the tree under
/// `hyper`.
struct Frames(UnboundedReceiver<Frame>);

impl futures_core::Stream for Frames {
    type Item = Result<SseEvent, std::convert::Infallible>;

    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        context: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        self.0.poll_recv(context).map(|frame| {
            frame.map(|frame| {
                Ok(match frame {
                    Frame::Named { kind, data } => SseEvent::default().event(kind).data(data),
                    Frame::Comment => SseEvent::default().comment("waiting for an answer"),
                })
            })
        })
    }
}

/// Run a turn, streaming its events, on the session's own thread.
fn run_turn_streaming(
    agent: &mut AgentSession,
    frames: &UnboundedSender<Frame>,
    session: &str,
    body: &str,
    pending: &Pending,
) {
    let message = match parse_message_body(body) {
        Ok(message) => message,
        Err(err) => {
            let (kind, data) = error_frame(&err);
            let _ = frames.send(Frame::Named {
                kind,
                data: data.to_string(),
            });
            return;
        }
    };

    let mut sink = SseSink {
        frames: frames.clone(),
        live: true,
    };
    let mut driver = PromptDriver {
        pending,
        frames: frames.clone(),
        session: session.to_string(),
    };
    if let RunResult::Failed(reason) =
        agent.run_streaming_with_driver(&mut driver, &mut sink, session, &message)
    {
        let (kind, data) = error_frame(&reason);
        let _ = frames.send(Frame::Named {
            kind,
            data: data.to_string(),
        });
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
    frames: UnboundedSender<Frame>,
    session: String,
}

impl Driver for PromptDriver<'_> {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        let (kind, payload) = prompt_frame(prompt, &self.session);
        if self
            .frames
            .send(Frame::Named {
                kind,
                data: payload.to_string(),
            })
            .is_err()
        {
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
            if self.frames.send(Frame::Comment).is_err() {
                // The receiver is gone, which means the client is: nobody
                // can answer. This is the heartbeat's other job, and the
                // only way a vanished client is noticed now that nothing
                // writes to a socket here.
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
fn authorised(headers: &HeaderMap, token: Option<&str>, path: &str) -> bool {
    let Some(expected) = token else { return true };
    // `/health` is probed by the supervisor without credentials.
    // The page and bundle are open because they carry no session data and grant
    // nothing. Clients need tokens to read sessions, send messages, or answer
    // prompts — all routes below. See test `an_api_route_still_refuses_without_a_token`.
    if matches!(path, "/health" | "/" | "/app.js") {
        return true;
    }
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .is_some_and(|presented| presented.trim() == expected)
}

/// Whether the client asked for an SSE stream (`Accept: text/event-stream`).
fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"))
}

/// The web client, compiled into the binary via `include_str!`. No directory
/// needed; resolves at **compile time**. `src/web/dist/` committed rather than
/// built in CI to avoid making Node a build dependency. See
/// `docs/concepts/architecture.md#user-interfaces-separate-clients` (#119).
const WEB_INDEX: &str = include_str!("../../web/dist/index.html");
const WEB_APP_JS: &str = include_str!("../../web/dist/app.js");

/// A static asset, served with its content type.
fn asset(body: &'static str, content_type: &'static str) -> Response {
    ([(axum::http::header::CONTENT_TYPE, content_type)], body).into_response()
}

impl IntoResponse for Reply {
    fn into_response(self) -> Response {
        (
            StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            [(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/json"),
            )],
            self.body,
        )
            .into_response()
    }
}

/// An [`EventSink`] that writes each event as an SSE frame to the socket.
struct SseSink {
    frames: UnboundedSender<Frame>,
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
        if self
            .frames
            .send(Frame::Named {
                kind,
                data: data.to_string(),
            })
            .is_err()
        {
            self.live = false;
            return Flow::Stop;
        }
        Flow::Continue
    }
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

fn error_reply(status: u16, message: &str) -> Reply {
    Reply {
        status,
        body: serde_json::json!({ "error": message }).to_string(),
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
