//! REST API surface over the `!Send` session thread; mid-turn confirmations bypass the job queue via [`crate::pending`].

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use tokio::sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender};

use jk_agent::conductor::RunResult;

use jk_agent::session::{
    answer_timeout, fork, new_session_id, session_payload, sessions_payload, Forked,
};
use jk_agent::{AgentSession, HeadlessDriver};
use jk_protocol::turn::{Driver, UserPrompt};
use jk_protocol::turn::{Event, EventSink, Flow};

use crate::pending::{Parked, Pending};
use crate::sessions::Agents;

/// Each tick writes an SSE comment (a no-op), so dead peers surface as write errors within one interval.
const HEARTBEAT: Duration = Duration::from_secs(5);

/// A ready HTTP reply: status code + JSON body.
pub struct Reply {
    /// HTTP status code.
    pub status: u16,
    /// JSON body.
    pub body: String,
}

/// A bound port, ready to serve; separate from serving so a test can learn the port first.
pub struct Surface {
    listener: std::net::TcpListener,
}

#[derive(Clone)]
struct App {
    agents: Arc<Agents>,
    pending: Arc<Pending>,
    principals: HashMap<String, String>,
    served: Arc<AtomicUsize>,
    /// How many responses to complete before shutting down; `usize::MAX` is the production loop.
    limit: usize,
    stop: Arc<tokio::sync::Notify>,
}

impl App {
    /// Runs `handler` on the session's thread; `spawn_blocking` because blocking a runtime worker for a whole turn would stall other requests.
    async fn on_session<F>(&self, session: Option<&str>, handler: F) -> Response
    where
        F: FnOnce(&mut AgentSession) -> Reply + Send + 'static,
    {
        let jobs = session.map_or_else(
            || self.agents.housekeeping(),
            |session| self.agents.of(session),
        );
        let reply = tokio::task::spawn_blocking(move || jobs.run(handler)).await;
        match reply {
            Ok(Ok(reply)) => reply.into_response(),
            Ok(Err(why)) => {
                error_reply(503, &format!("the session is unavailable: {why}")).into_response()
            }
            Err(_) => error_reply(500, "the request was cancelled").into_response(),
        }
    }

    fn served_one(&self) {
        if self.limit == usize::MAX {
            return;
        }
        if self.served.fetch_add(1, Ordering::Relaxed) + 1 >= self.limit {
            self.stop.notify_one();
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
    /// If the listener has no local address, which a bound socket always has.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.listener.local_addr().expect("a bound listener").port()
    }

    /// Serves one request then returns; a stopping surface may drop the next connection, so use [`Surface::serve_while`] for several.
    ///
    /// # Errors
    /// Whatever the server failed with.
    pub fn serve_once(&self, agents: &Arc<Agents>) -> std::io::Result<()> {
        self.run(agents, HashMap::new(), 1, None::<fn(u16)>)
    }

    /// Serves one request, requiring `Bearer <token>` when one is set.
    ///
    /// # Errors
    /// Whatever the server failed with.
    pub fn serve_once_authed(
        &self,
        agents: &Arc<Agents>,
        principals: HashMap<String, String>,
    ) -> std::io::Result<()> {
        self.run(agents, principals, 1, None::<fn(u16)>)
    }

    /// Serve while `client` runs, then stop, returning what it returned.
    ///
    /// # Panics
    /// Propagates a panic from `client` rather than hanging on it.
    pub fn serve_while<F, R>(
        &self,
        agents: &Arc<Agents>,
        principals: HashMap<String, String>,
        client: F,
    ) -> R
    where
        F: FnOnce(u16) -> R + Send,
        R: Send,
    {
        let answer = std::sync::Mutex::new(None);
        let _ = self.run(
            agents,
            principals,
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
        agents: &Arc<Agents>,
        principals: HashMap<String, String>,
    ) -> std::io::Result<()> {
        self.run(agents, principals, usize::MAX, None::<fn(u16)>)
    }

    /// The loop all four entry points share; `alongside` runs while it serves.
    fn run<F>(
        &self,
        agents: &Arc<Agents>,
        principals: HashMap<String, String>,
        limit: usize,
        alongside: Option<F>,
    ) -> std::io::Result<()>
    where
        F: FnOnce(u16) + Send,
    {
        let app = App {
            agents: Arc::clone(agents),
            pending: Arc::new(Pending::new()),
            principals,
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
                alongside(port);
                stop.notify_one();
            }
            serving.join()
        });
        outcome.unwrap_or_else(|_| Err(std::io::Error::other("the serving thread panicked")))
    }
}

fn router(app: App) -> Router {
    Router::new()
        .route("/health", get(health_route))
        .route("/sessions", get(list_sessions).post(create_session))
        .route("/contributions", get(contributions))
        .route("/contributions/invoke", post(invoke))
        .route("/session/{id}", get(get_session))
        .route("/session/{id}/message", post(message))
        .route("/session/{id}/answer", post(answer))
        .route("/session/{id}/fork", post(fork_session))
        .route("/ws", get(socket))
        .fallback(not_found)
        .layer(axum::middleware::from_fn_with_state(app.clone(), guard))
        .with_state(app)
}

/// Bearer-token check and response counter in one layer: every route passes through, so a new route is covered by auth.
async fn guard(
    State(app): State<App>,
    mut request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if let Some(principal) = authorised(request.headers(), &app.principals, &path) {
        request.extensions_mut().insert(principal);
        let response = next.run(request).await;
        app.served_one();
        response
    } else {
        app.served_one();
        error_reply(401, "missing or invalid bearer token").into_response()
    }
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
    app.on_session(None, |agent: &mut AgentSession| handle_list_sessions(agent))
        .await
}

async fn contributions(State(app): State<App>) -> Response {
    app.on_session(None, handle_contributions).await
}

async fn invoke(State(app): State<App>, body: String) -> Response {
    app.on_session(None, move |agent| handle_invoke(agent, &body))
        .await
}

async fn get_session(State(app): State<App>, Path(id): Path<String>) -> Response {
    app.on_session(None, move |agent| handle_get_session(agent, &id))
        .await
}

async fn fork_session(State(app): State<App>, Path(id): Path<String>, body: String) -> Response {
    app.on_session(None, move |agent| handle_fork_session(agent, &id, &body))
        .await
}

/// `POST /session/{id}/answer`, handled off the session thread, where it would queue behind the parked turn.
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
                error_reply(409, "no confirmation is pending").into_response()
            }
        }
    }
}

/// `GET /ws`, upgraded after `guard` so token rules match REST.
async fn socket(State(app): State<App>, upgrade: axum::extract::ws::WebSocketUpgrade) -> Response {
    let agents = Arc::clone(&app.agents);
    upgrade.on_upgrade(move |socket| crate::ws::serve_socket(socket, agents))
}

async fn message(
    State(app): State<App>,
    Path(id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let principal = authorised(&headers, &app.principals, "");
    if !accepts_event_stream(&headers) {
        return app
            .on_session(Some(&id.clone()), move |agent| {
                handle_message(agent, &id, &body, principal.clone())
            })
            .await;
    }
    let (frames, stream) = unbounded_channel::<Frame>();
    let pending = Arc::clone(&app.pending);
    let queued = app.agents.of(&id).send(move |agent: &mut AgentSession| {
        run_turn_streaming(agent, &frames, &id, &body, &pending, principal);
    });
    if queued.is_err() {
        return error_reply(503, "the session is unavailable").into_response();
    }
    Sse::new(Frames(stream)).into_response()
}

/// `GET /health` — liveness probe.
#[must_use]
pub fn health() -> Reply {
    Reply {
        status: 200,
        body: serde_json::json!({ "status": "ok", "version": env!("CARGO_PKG_VERSION") })
            .to_string(),
    }
}

fn handle_contributions(agent: &mut AgentSession) -> Reply {
    Reply {
        status: 200,
        body: contributions_payload(agent).to_string(),
    }
}

/// The answer carries `contributions-changed`, the only moment the set can move while a session is up.
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
    let arguments: Vec<jk_protocol::contributions::ArgumentValue> = value
        .get("arguments")
        .and_then(serde_json::Value::as_array)
        .map(|given| {
            given
                .iter()
                .filter_map(|argument| {
                    Some(jk_protocol::contributions::ArgumentValue {
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
        Err(error @ jk_protocol::contributions::InvokeError::Unknown) => {
            error_reply(404, &error.to_string())
        }
        Err(error @ jk_protocol::contributions::InvokeError::InvalidArguments) => {
            error_reply(400, &error.to_string())
        }
        Err(error @ jk_protocol::contributions::InvokeError::Failed(_)) => {
            error_reply(500, &error.to_string())
        }
    }
}

fn handle_list_sessions(agent: &AgentSession) -> Reply {
    Reply {
        status: 200,
        body: sessions_payload(agent).to_string(),
    }
}

fn handle_create_session() -> Reply {
    let id = new_session_id();
    Reply {
        status: 201,
        body: serde_json::json!({ "id": id }).to_string(),
    }
}

/// The child id is generated here so client-picked ids cannot collide with live sessions.
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

fn parse_at_seq(body: &str) -> Result<u64, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    value
        .get("at-seq")
        .ok_or("missing field `at-seq`")?
        .as_u64()
        .ok_or_else(|| "`at-seq` must be a non-negative whole number".to_owned())
}

fn handle_get_session(agent: &AgentSession, id: &str) -> Reply {
    Reply {
        status: 200,
        body: session_payload(agent, id).to_string(),
    }
}

fn handle_message(
    agent: &mut AgentSession,
    session: &str,
    body: &str,
    principal: Option<String>,
) -> Reply {
    let message = match parse_message_body(body) {
        Ok(m) => m,
        Err(err) => return error_reply(400, &err),
    };
    let mut driver = HeadlessDriver;
    match agent.run_with_driver_principal(&mut driver, session, &message, principal) {
        RunResult::Answered { text, agentic } => Reply {
            status: 200,
            body: serde_json::json!({ "answer": text, "agentic": agentic }).to_string(),
        },
        RunResult::Failed(reason) => error_reply(502, &reason),
    }
}

/// One SSE frame headed to a client; a closed receiver is how a vanished client is noticed.
enum Frame {
    Named { kind: &'static str, data: String },
    Comment,
}

/// Hand-written to avoid a `tokio-stream` dependency.
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

fn run_turn_streaming(
    agent: &mut AgentSession,
    frames: &UnboundedSender<Frame>,
    session: &str,
    body: &str,
    pending: &Pending,
    principal: Option<String>,
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
    if let RunResult::Failed(reason) = agent.run_streaming_with_driver_principal(
        &mut driver,
        &mut sink,
        session,
        &message,
        principal,
    ) {
        let (kind, data) = error_frame(&reason);
        let _ = frames.send(Frame::Named {
            kind,
            data: data.to_string(),
        });
    }
}

/// Puts a question to the client over the open SSE stream; the answer arrives as its own request via [`Pending`].
struct PromptDriver<'a> {
    pending: &'a Pending,
    frames: UnboundedSender<Frame>,
    session: String,
}

impl Driver for PromptDriver<'_> {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        let Some(parked) = self.pending.park(&self.session) else {
            return prompt.default_answer.clone();
        };
        let (kind, payload) = prompt_frame(prompt, &self.session);
        if self
            .frames
            .send(Frame::Named {
                kind,
                data: payload.to_string(),
            })
            .is_err()
        {
            return prompt.default_answer.clone();
        }
        self.wait_for_answer(&parked)
            .unwrap_or_else(|| prompt.default_answer.clone())
    }
}

impl PromptDriver<'_> {
    /// The deadline lives here, not in [`Pending`]: the heartbeat is a write, so waking every `HEARTBEAT` notices a vanished client early.
    fn wait_for_answer(&self, parked: &Parked<'_>) -> Option<String> {
        let deadline = Instant::now() + answer_timeout();
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            let slice = remaining.min(HEARTBEAT);
            if let Some(answer) = parked.wait(slice) {
                return Some(answer);
            }
            if remaining <= slice {
                return None;
            }
            if self.frames.send(Frame::Comment).is_err() {
                return None;
            }
        }
    }
}

/// A `None` token allows all (default bind is loopback); a set token is required everywhere except `/health`, which the supervisor probes.
fn authorised(
    headers: &HeaderMap,
    principals: &HashMap<String, String>,
    path: &str,
) -> Option<String> {
    if path == "/health" {
        return Some(String::new());
    }

    if principals.is_empty() {
        return Some(String::new());
    }

    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .strip_prefix("Bearer ")
                .or_else(|| value.strip_prefix("bearer "))
        })
        .and_then(|presented| {
            principals
                .iter()
                .find(|(_, token)| token.trim() == presented.trim())
                .map(|(principal, _)| principal.clone())
        })
}

fn accepts_event_stream(headers: &HeaderMap) -> bool {
    headers
        .get(axum::http::header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/event-stream"))
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

struct SseSink {
    frames: UnboundedSender<Frame>,
    live: bool,
}

/// The SSE frame a turn event projects to: its `event:` kind and `data` JSON.
#[must_use]
pub fn sse_frame(event: &Event) -> (&'static str, serde_json::Value) {
    match event {
        Event::TextDelta(text) => ("delta", serde_json::json!({ "text": text })),
        Event::ToolInvoked(call) => (
            "tool",
            serde_json::json!({ "id": call.id, "name": call.name, "arguments": call.arguments }),
        ),
        Event::ToolResult(outcome) => (
            "tool-result",
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

/// The `prompt` frame: an interceptor's question.
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

/// The `error` frame, shared so call sites cannot drift.
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

fn parse_answer_body(body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|e| format!("invalid JSON body: {e}"))?;
    let answer = value
        .get("answer")
        .and_then(serde_json::Value::as_str)
        .ok_or("missing string field `answer`")?;
    Ok(answer.to_string())
}

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

/// One object per extension in load order, with the protocol's `surface/contributions` field names.
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
    use super::{health, unbounded_channel, Frame, Pending, PromptDriver};
    use jk_protocol::turn::{Driver, UserPrompt};

    /// The frame was sent before the turn parked, so an answering client could find nothing pending and get `409`.
    #[test]
    fn a_prompt_is_answerable_as_soon_as_its_frame_can_be_read() {
        for _ in 0..200 {
            let pending = Pending::new();
            let (frames, mut received) = unbounded_channel();
            let refused = std::thread::scope(|scope| {
                let asked = scope.spawn(|| {
                    let mut driver = PromptDriver {
                        pending: &pending,
                        frames,
                        session: "s1".to_owned(),
                    };
                    driver.ask(&UserPrompt {
                        question: "Run it?".to_owned(),
                        options: vec!["yes".to_owned(), "no".to_owned()],
                        default_answer: "no".to_owned(),
                    })
                });
                let frame = received.blocking_recv().expect("the prompt frame is sent");
                assert!(matches!(frame, Frame::Named { .. }), "a named prompt frame");
                let refused = !pending.answer("s1", "yes".to_owned());
                if refused {
                    while !pending.answer("s1", "yes".to_owned()) {
                        std::thread::yield_now();
                    }
                }
                assert_eq!(asked.join().expect("the turn ends"), "yes");
                refused
            });
            assert!(
                !refused,
                "the frame was readable before the turn parked, so its answer was refused"
            );
        }
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
