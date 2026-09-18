//! Newline-delimited JSON-RPC 2.0 over a byte stream—transport for spawned
//! clients (no server, port, or token).
//!
//! One frame per line. The client owns the process; it dies with them.
//! Editors speak this shape (LSP, Codex's `app-server`), so Phase 18's MCP and
//! ACP adapt *this* rather than REST.
//!
//! # Generic over streams, on purpose
//!
//! [`serve`] takes any [`BufRead`] and [`Write`], not `stdin()`/`stdout()`.
//! Gateway passes real ones; tests pass pipes and buffers for in-process
//! exchanges without subprocess or model. Slice 13c's WebSocket does the same
//! dispatch over different streams.
//!
//! # Cancellation mid-turn
//!
//! A reader thread hands input lines over a channel. While a turn runs, this
//! thread is in the conductor, so `turn/cancel` needs someone else holding the
//! pipe to be read. The main thread picks queued frames between turn events
//! (in [`RpcSink::emit`]) and cancels by returning [`Flow::Stop`]—the same
//! cancellation as an SSE client disconnecting.
//!
//! `AgentSession` is `!Send` and stays on this thread, so nothing is locked:
//! sink and driver share the writer via `Rc<RefCell<_>>`.
//!
//! # Handshake: required, not optional
//!
//! `protocol/hello` must be the first frame. A skippable negotiation negotiates
//! nothing: a client built against a version this core can't speak would
//! otherwise work, defeating the version check. Incompatible clients are told
//! what this core speaks, then the connection closes.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Instant;

use jan_klod_core::contributions;
use jan_klod_protocol::{
    compatible, jsonrpc, Command, HelloResult, Notification, SurfaceInvokeResult, COMMAND_METHODS,
    PROTOCOL_VERSION,
};

use jan_klod_core::conductor::{Event, EventSink, Flow, RunResult};
use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::session::{self, Forked};
use jan_klod_core::AgentSession;

/// Serve frames from `input`, answering on `output`, until EOF.
///
/// Returns `Ok(())` on client departure (EOF) or refused handshake. Malformed
/// frames are answered and the loop continues—one bad client shouldn't take
/// the session down.
///
/// # Errors
/// Propagates write failures: the pipe is gone, no one left to answer.
pub fn serve<R: BufRead + Send + 'static, W: Write>(
    input: R,
    output: W,
    agent: &mut AgentSession,
) -> std::io::Result<()> {
    let (handover, incoming) = mpsc::channel();
    // Reader thread hands lines over; holds no session (AgentSession is !Send).
    // This division lets mid-turn `turn/cancel` be readable. While a turn runs,
    // this thread is in the conductor, so something else must hold the pipe or
    // cancels are delayed. Nothing is locked—only this thread shares state.
    std::thread::spawn(move || {
        for line in input.lines() {
            let handed = match line {
                Ok(line) => handover.send(Incoming::Line(line)),
                // Non-UTF-8 is the frame's problem, not the stream's
                Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                    handover.send(Incoming::NotUtf8)
                }
                // Pipe broke; receiver learns from channel closing
                Err(_) => break,
            };
            if handed.is_err() {
                break; // the loop below is gone
            }
        }
    });

    let wire = Wire {
        writer: Rc::new(RefCell::new(output)),
        incoming: &incoming,
    };
    let mut negotiated = false;
    // recv ends when reader thread drops its end (EOF)
    while let Ok(frame) = wire.incoming.recv() {
        let line = match frame {
            Incoming::Line(line) => line,
            Incoming::NotUtf8 => {
                wire.write(&refuse(
                    jsonrpc::Id::Null,
                    jsonrpc::PARSE_ERROR,
                    "a frame must be UTF-8".to_owned(),
                ))?;
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let served = match parse(&line) {
            Ok(request) => command(request.command, request.id, &mut negotiated, agent, &wire),
            Err(refusal) => Served::Answer(refusal),
        };
        match served {
            Served::Answer(response) => wire.write(&response)?,
            Served::Close(response) => {
                wire.write(&response)?;
                return Ok(());
            }
        }
    }
    // Reader thread ends itself; joining would block on a read with no reason to
    // return (refused handshake hangs while client writes). Process exiting anyway.
    Ok(())
}

/// From the reader thread.
pub(crate) enum Incoming {
    /// Text line.
    Line(String),
    /// Non-UTF-8 bytes.
    NotUtf8,
}

/// Connection ends: where answers go and frames arrive.
pub(crate) struct Wire<'a, W: Write> {
    /// Shared by turn's sink and driver. `Rc<RefCell<_>>` not a lock—single
    /// thread writes.
    writer: Rc<RefCell<W>>,
    incoming: &'a Receiver<Incoming>,
}

impl<'a, W: Write> Wire<'a, W> {
    /// A wire over `writer`, reading frames from `incoming`.
    ///
    /// Public to the crate since #226: the WebSocket surface dispatches
    /// through [`command`] rather than re-implementing the protocol, and
    /// that needs the same wire — one writer for answers and notifications,
    /// one receiver a running turn can poll for `turn/cancel`.
    pub(crate) const fn new(writer: Rc<RefCell<W>>, incoming: &'a Receiver<Incoming>) -> Self {
        Self { writer, incoming }
    }
}

impl<W: Write> Wire<'_, W> {
    /// Answer one request.
    pub(crate) fn write(&self, response: &jsonrpc::Response) -> std::io::Result<()> {
        write_frame(&mut *self.writer.borrow_mut(), response)
    }
}

/// What serving one frame decided.
pub(crate) enum Served {
    /// Answer it and read the next frame.
    Answer(jsonrpc::Response),
    /// Answer it and hang up. Only a refused handshake does this.
    Close(jsonrpc::Response),
}

/// Which session a command is about, if any (#229).
///
/// A WebSocket connection is not bound to one session — its frames name
/// them — so the socket has to route each frame to the agent that owns
/// the session it names. Written here beside the command enum so a new
/// variant is a compile error in one place rather than a frame silently
/// served by the wrong agent.
#[must_use]
pub(crate) fn session_of(command: &Command) -> Option<&str> {
    match command {
        Command::SessionGet { session }
        | Command::SessionFork { session, .. }
        | Command::SessionMessage { session, .. }
        | Command::TurnAnswer { session, .. }
        | Command::TurnCancel { session }
        | Command::TurnFollowUp { session, .. } => Some(session),
        // Negotiation, creation, listing and a surface invocation belong
        // to no session: the store is shared, so any agent answers them
        // the same way.
        Command::Hello { .. }
        | Command::SessionCreate
        | Command::SessionList
        | Command::SurfaceInvoke { .. } => None,
    }
}

/// The refusal for a frame that is not text (#226).
///
/// Beside `parse`'s refusals rather than in the socket, so every "this is
/// not a frame of this protocol" answer is shaped the same way.
#[must_use]
pub(crate) fn not_text() -> String {
    let response = refuse(
        jsonrpc::Id::Null,
        jsonrpc::PARSE_ERROR,
        "a frame must be text, one JSON object".to_owned(),
    );
    serde_json::to_string(&response).unwrap_or_else(|_| String::from("{}"))
}

/// Parse a line to a request or refusal.
///
/// Separate from serving so frame rules can be tested without an `AgentSession`,
/// booted runtime, staged `ext/`, or model.
pub(crate) fn parse(line: &str) -> Result<jsonrpc::Request, jsonrpc::Response> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Err(refuse(
            jsonrpc::Id::Null,
            jsonrpc::PARSE_ERROR,
            "a frame must be one JSON object on one line".to_owned(),
        ));
    };
    // Happy path uses contract type so envelope is per `jan-klod-protocol`, not a
    // second reading. Failures use the raw value only to explain why.
    match serde_json::from_str::<jsonrpc::Request>(line) {
        // `Id::Null` exists so *this core* can answer a frame it could not read
        // an id from. A client that sends one is asking for an answer it cannot
        // match to anything it sent, which the spec discourages and this
        // refuses outright — an uncorrelatable answer is worse than none.
        Ok(request) if request.id == jsonrpc::Id::Null => Err(refuse(
            jsonrpc::Id::Null,
            jsonrpc::INVALID_REQUEST,
            "`id` must not be null: an answer to it could not be matched to the \
             request that earned it"
                .to_owned(),
        )),
        Ok(request) if request.jsonrpc == jsonrpc::VERSION => Ok(request),
        Ok(request) => Err(refuse(
            request.id,
            jsonrpc::INVALID_REQUEST,
            format!(
                "`jsonrpc` must be \"{}\", not \"{}\"",
                jsonrpc::VERSION,
                request.jsonrpc
            ),
        )),
        Err(err) => Err(diagnose(&value, &err)),
    }
}

/// Why a frame that is valid JSON is not a request this core can serve.
///
/// Distinguish "no such command" from "wrong arguments" (serde gives one code,
/// JSON-RPC two). Method name tells them apart via [`COMMAND_METHODS`].
fn diagnose(value: &serde_json::Value, err: &serde_json::Error) -> jsonrpc::Response {
    let Some(object) = value.as_object() else {
        return refuse(
            jsonrpc::Id::Null,
            jsonrpc::INVALID_REQUEST,
            "a request is a JSON object".to_owned(),
        );
    };
    // ID is read before method, its absence is reported as absence. Reversed,
    // a known method with no ID was `invalid params`, misdirecting clients.
    let id = match object.get("id") {
        None => {
            return refuse(
                jsonrpc::Id::Null,
                jsonrpc::INVALID_REQUEST,
                "every request carries an `id`: this transport has no client-to-core \
                 notifications, so a frame nothing can answer is a mistake"
                    .to_owned(),
            )
        }
        Some(raw) => match serde_json::from_value::<jsonrpc::Id>(raw.clone()) {
            Ok(id) => id,
            Err(_) => {
                return refuse(
                    jsonrpc::Id::Null,
                    jsonrpc::INVALID_REQUEST,
                    format!("`id` must be a number or a string, not {raw}"),
                )
            }
        },
    };
    let Some(method) = object.get("method").and_then(serde_json::Value::as_str) else {
        return refuse(
            id,
            jsonrpc::INVALID_REQUEST,
            "a request carries a `method`".to_owned(),
        );
    };
    if COMMAND_METHODS.contains(&method) {
        refuse(
            id,
            jsonrpc::INVALID_PARAMS,
            format!("`{method}` was sent with the wrong params: {err}"),
        )
    } else {
        refuse(
            id,
            jsonrpc::METHOD_NOT_FOUND,
            format!("no command is named `{method}`"),
        )
    }
}

/// Serve one command.
///
/// Only handshake and the guard are unconditional; every arm names a command
/// (no wildcard), so new protocol commands fail to compile until the transport
/// handles them. Guarded arms don't count as exhaustive, letting the handshake
/// gate sit mid-match instead of repeating as an early return.
pub(crate) fn command<W: Write>(
    command: Command,
    id: jsonrpc::Id,
    negotiated: &mut bool,
    agent: &mut AgentSession,
    wire: &Wire<W>,
) -> Served {
    match command {
        Command::Hello { version } => {
            if compatible(PROTOCOL_VERSION, &version) {
                *negotiated = true;
                // What the extensions offer, before the client asks. Sent
                // only when something is contributed: an absent notification
                // and an empty one say the same thing, and the common case is
                // that nothing contributes at all. A client that renders none
                // of it ignores the frame and runs turns unchanged
                // (`wit/client-surface.wit`), so a write failure here is not
                // worth failing the handshake over.
                let Notification::SurfaceContributions { extensions } =
                    contributions_notification(agent)
                else {
                    unreachable!("contributions_notification builds that variant")
                };
                if !extensions.is_empty() {
                    let _ = write_notification(
                        &mut *wire.writer.borrow_mut(),
                        &Notification::SurfaceContributions { extensions },
                    );
                }
                answer(
                    id,
                    serde_json::to_value(HelloResult::default()).unwrap_or_default(),
                )
            } else {
                Served::Close(jsonrpc::Response::error(
                    id,
                    jsonrpc::incompatible_version(&version),
                ))
            }
        }
        // Everything past handshake requires handshake
        _ if !*negotiated => Served::Answer(refuse(
            id,
            jsonrpc::INVALID_REQUEST,
            "send `protocol/hello` first".to_owned(),
        )),
        Command::SessionCreate => {
            answer(id, serde_json::json!({ "id": session::new_session_id() }))
        }
        Command::SessionList => answer(id, session::sessions_payload(agent)),
        Command::SessionGet { session } => answer(id, session::session_payload(agent, &session)),
        Command::SessionFork { session, at_seq } => match session::fork(agent, &session, at_seq) {
            Forked::Created(payload) => answer(id, payload),
            // Requested events don't exist—caller's arguments, not core failure
            Forked::Empty(message) => Served::Answer(refuse(id, jsonrpc::INVALID_PARAMS, message)),
            Forked::Failed(message) => Served::Answer(refuse(id, jsonrpc::INTERNAL_ERROR, message)),
        },
        Command::SessionMessage { session, message } => turn(&session, &message, id, agent, wire),
        // These steer running turns (served in `turn` above). This thread
        // reaching them means no turn exists—nothing to answer, cancel, steer.
        Command::TurnAnswer { .. } => Served::Answer(refuse(
            id,
            jsonrpc::INVALID_REQUEST,
            "no confirmation is pending".to_owned(),
        )),
        Command::TurnCancel { .. } => Served::Answer(refuse(
            id,
            jsonrpc::INVALID_REQUEST,
            "no turn is running".to_owned(),
        )),
        Command::TurnFollowUp { .. } => Served::Answer(refuse(
            id,
            jsonrpc::INVALID_REQUEST,
            "no turn is running to steer: send `session/message` to start one".to_owned(),
        )),
        Command::SurfaceInvoke {
            extension,
            name,
            arguments,
        } => {
            let arguments: Vec<contributions::ArgumentValue> = arguments
                .into_iter()
                .map(|argument| contributions::ArgumentValue {
                    name: argument.name,
                    value: argument.value,
                })
                .collect();
            match agent.invoke_contribution(&extension, &name, &arguments) {
                Ok(outcome) => {
                    // The set moved, so every client's copy is stale. Told
                    // rather than polled, and told before the answer, so a
                    // client that re-renders on the notification has the new
                    // set in hand when the result arrives. Sent even when it
                    // is now empty — unlike at connect, because a client
                    // showing items that have just gone away has to hear it.
                    if outcome.contributions_changed {
                        let declared = contributions_notification(agent);
                        let _ = write_notification(&mut *wire.writer.borrow_mut(), &declared);
                    }
                    answer(
                        id,
                        serde_json::to_value(SurfaceInvokeResult {
                            text: outcome.text,
                            contributions_changed: outcome.contributions_changed,
                        })
                        .unwrap_or_default(),
                    )
                }
                // A name nobody contributes is the caller's mistake; the
                // other two are the extension's, and the codes say which.
                Err(error @ contributions::InvokeError::Unknown) => {
                    Served::Answer(refuse(id, jsonrpc::METHOD_NOT_FOUND, error.to_string()))
                }
                Err(error @ contributions::InvokeError::InvalidArguments) => {
                    Served::Answer(refuse(id, jsonrpc::INVALID_PARAMS, error.to_string()))
                }
                Err(error @ contributions::InvokeError::Failed(_)) => {
                    Served::Answer(refuse(id, jsonrpc::INTERNAL_ERROR, error.to_string()))
                }
            }
        }
    }
}

/// Run one turn, streaming events as notifications.
///
/// Request is answered when the turn ends with `{answer, agentic}` (same as
/// blocking REST). Deliberate duplication of `done`: clients can await the
/// response or render the stream, not both.
fn turn<W: Write>(
    session: &str,
    message: &str,
    id: jsonrpc::Id,
    agent: &mut AgentSession,
    wire: &Wire<W>,
) -> Served {
    let state = Rc::new(TurnState {
        session: session.to_owned(),
        cancelled: Cell::new(false),
        follow_ups: RefCell::new(VecDeque::new()),
        asking: Cell::new(false),
    });
    let mut sink = RpcSink {
        writer: Rc::clone(&wire.writer),
        incoming: wire.incoming,
        state: Rc::clone(&state),
    };
    let mut driver = RpcDriver {
        writer: Rc::clone(&wire.writer),
        incoming: wire.incoming,
        state: Rc::clone(&state),
    };
    match agent.run_streaming_with_driver(&mut driver, &mut sink, session, message) {
        RunResult::Answered { text, agentic } => answer(
            id,
            serde_json::json!({ "answer": text, "agentic": agentic }),
        ),
        // A failed turn is reported once, here, against the id that asked for
        // it. The `error` notification exists for SSE, which has no id to
        // answer and therefore nothing else to report through.
        RunResult::Failed(reason) => Served::Answer(refuse(id, jsonrpc::INTERNAL_ERROR, reason)),
    }
}

/// What the sink and the driver both need to know about the running turn.
struct TurnState {
    /// The session this turn belongs to. A `turn/cancel` naming another one is
    /// refused rather than applied — the client has lost track of which turn is
    /// running, and cancelling the wrong one silently would be worse.
    session: String,
    cancelled: Cell<bool>,
    follow_ups: RefCell<VecDeque<String>>,
    /// Whether a question is on the wire waiting to be answered.
    ///
    /// An answer that arrives when nothing asked is refused, not stashed. REST
    /// refuses it too (`409 no confirmation is pending`), and for a permission
    /// prompt the reason is worth stating: a stashed answer would sit there
    /// until the *next* question and approve it, which is how "yes" to reading
    /// a file becomes "yes" to running a command.
    asking: Cell<bool>,
}

/// Streams a turn's events, and notices a cancel between them.
struct RpcSink<'a, W: Write> {
    writer: Rc<RefCell<W>>,
    incoming: &'a Receiver<Incoming>,
    state: Rc<TurnState>,
}

impl<W: Write> EventSink for RpcSink<'_, W> {
    fn emit(&mut self, event: &Event) -> Flow {
        if write_notification(&mut *self.writer.borrow_mut(), &notification_for(event)).is_err() {
            // Nobody is reading, so there is nothing left to stream.
            return Flow::Stop;
        }
        // Between two events is where a cancel gets read. The conductor checks
        // the returned `Flow` at its own loop boundaries, so `Stop` here is the
        // same cancellation an SSE client gets by disconnecting — no new
        // mechanism, and no conductor change.
        serve_queued(self.incoming, &self.state, &self.writer);
        if self.state.cancelled.get() {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }
}

/// Puts a question to the client and waits, on this thread, for the answer to
/// arrive as its own frame.
struct RpcDriver<'a, W: Write> {
    writer: Rc<RefCell<W>>,
    incoming: &'a Receiver<Incoming>,
    state: Rc<TurnState>,
}

impl<W: Write> Driver for RpcDriver<'_, W> {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        let question = Notification::Ask {
            session: self.state.session.clone(),
            question: prompt.question.clone(),
            options: prompt.options.clone(),
            default: prompt.default_answer.clone(),
        };
        if write_notification(&mut *self.writer.borrow_mut(), &question).is_err() {
            // The client is gone; nobody can answer, so take the safe default.
            return prompt.default_answer.clone();
        }
        self.wait_for_answer()
            .unwrap_or_else(|| prompt.default_answer.clone())
    }

    fn follow_up(&mut self) -> Option<String> {
        // The turn is about to end, so this is the last chance to notice a
        // steering message that arrived while it was running.
        serve_queued(self.incoming, &self.state, &self.writer);
        self.state.follow_ups.borrow_mut().pop_front()
    }
}

impl<W: Write> RpcDriver<'_, W> {
    /// Read frames until this session's answer arrives, or the wait expires.
    ///
    /// Simpler than the REST equivalent by one whole mechanism: there, a client
    /// that vanished leaves a socket that looks fine, so the wait has to poke it
    /// with a heartbeat to find out. Here the pipe closing closes the channel,
    /// and `recv_timeout` reports that as `Disconnected` — a real signal
    /// instead of a probe.
    fn wait_for_answer(&self) -> Option<String> {
        let deadline = Instant::now() + session::answer_timeout();
        // Only while parked here is an answer something to take.
        self.state.asking.set(true);
        let answer = self.wait_until(deadline);
        self.state.asking.set(false);
        answer
    }

    /// The wait itself, so the `asking` flag is cleared on every path out.
    fn wait_until(&self, deadline: Instant) -> Option<String> {
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match self.incoming.recv_timeout(remaining) {
                Ok(frame) => {
                    if let Some(answer) = serve_frame(frame, &self.state, &self.writer) {
                        return Some(answer);
                    }
                    // A cancel while parked: stop waiting and let the turn end
                    // at the next boundary, taking the prompt's own default —
                    // which for the permission gate is a denial.
                    if self.state.cancelled.get() {
                        return None;
                    }
                }
                // Timed out, or the client went away mid-question.
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

/// Serve every frame already queued, without waiting for more.
fn serve_queued<W: Write>(
    incoming: &Receiver<Incoming>,
    state: &TurnState,
    writer: &Rc<RefCell<W>>,
) {
    while let Ok(frame) = incoming.try_recv() {
        // An answer arriving when nothing asked is refused inside `serve_frame`
        // and returns `None`, so nothing is stashed for a question that never
        // came.
        drop(serve_frame(frame, state, writer));
    }
}

/// Serve one frame that arrived while a turn is running.
///
/// Returns the text of a `turn/answer` for this session, if that is what it
/// was. Everything else is answered in place: the core is mid-turn and
/// single-threaded, and a client told "busy" can retry, while one left hanging
/// cannot.
fn serve_frame<W: Write>(
    frame: Incoming,
    state: &TurnState,
    writer: &Rc<RefCell<W>>,
) -> Option<String> {
    let mut answer_text = None;
    let response = match frame {
        Incoming::NotUtf8 => refuse(
            jsonrpc::Id::Null,
            jsonrpc::PARSE_ERROR,
            "a frame must be UTF-8".to_owned(),
        ),
        Incoming::Line(line) if line.trim().is_empty() => return None,
        Incoming::Line(line) => match parse(&line) {
            Err(refusal) => refusal,
            Ok(request) => {
                let id = request.id;
                match request.command {
                    Command::TurnAnswer { session, answer }
                        if session == state.session && state.asking.get() =>
                    {
                        answer_text = Some(answer);
                        jsonrpc::Response::result(id, serde_json::json!({ "accepted": true }))
                    }
                    Command::TurnAnswer { session, .. } if session == state.session => refuse(
                        id,
                        jsonrpc::INVALID_REQUEST,
                        "no confirmation is pending".to_owned(),
                    ),
                    Command::TurnCancel { session } if session == state.session => {
                        state.cancelled.set(true);
                        jsonrpc::Response::result(id, serde_json::json!({ "cancelling": true }))
                    }
                    Command::TurnFollowUp { session, message } if session == state.session => {
                        state.follow_ups.borrow_mut().push_back(message);
                        jsonrpc::Response::result(id, serde_json::json!({ "queued": true }))
                    }
                    // Named the wrong session, or is not a mid-turn command at
                    // all. Distinguishing the two would not help the client:
                    // either way this turn is what is running.
                    _ => refuse(
                        id,
                        jsonrpc::INVALID_REQUEST,
                        format!(
                            "session `{}` is mid-turn: answer, cancel or steer it, or retry \
                             when it finishes",
                            state.session
                        ),
                    ),
                }
            }
        },
    };
    let _ = write_frame(&mut *writer.borrow_mut(), &response);
    answer_text
}

/// The notification each turn event becomes.
///
/// Exhaustive by construction — no `_ =>` arm — so a new `Event` variant stops
/// this from compiling until it has somewhere to go. It lived in
/// `core/tests/protocol_events.rs` while no transport existed; that test now
/// asserts *this* function rather than a copy of it, which is the only version
/// of the assertion worth having.
#[must_use]
pub fn notification_for(event: &Event) -> Notification {
    match event {
        Event::TextDelta(text) => Notification::TextDelta { text: text.clone() },
        Event::ToolInvoked(call) => Notification::ToolInvoked {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        },
        Event::ToolResult(outcome) => Notification::ToolResult {
            id: outcome.tool_call_id.clone(),
            content: outcome.content.clone(),
            failed: outcome.failed,
        },
        Event::Warning(message) => Notification::Warning {
            message: message.clone(),
        },
        Event::Done { text, agentic } => Notification::Done {
            answer: text.clone(),
            agentic: *agentic,
        },
    }
}

/// Write one notification: the same line discipline as a response, and no id,
/// because nothing answers it.
fn write_notification<W: Write>(
    output: &mut W,
    notification: &Notification,
) -> std::io::Result<()> {
    let framed = jsonrpc::Notification::new(notification.clone());
    let text = serde_json::to_string(&framed).expect("a notification serializes");
    output.write_all(text.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

/// A successful answer.
fn answer(id: jsonrpc::Id, result: serde_json::Value) -> Served {
    Served::Answer(jsonrpc::Response::result(id, result))
}

/// A failed answer.
fn refuse(id: jsonrpc::Id, code: i64, message: String) -> jsonrpc::Response {
    jsonrpc::Response::error(id, jsonrpc::Error::new(code, message))
}

/// Write one frame and **flush it**.
///
/// The flush is the whole function. Stdout to a pipe is block-buffered, so
/// without it a client waits for an answer that is sitting in this process's
/// buffer — and it would be a client that hangs only when not attached to a
/// terminal, which is every client.
fn write_frame<W: Write>(output: &mut W, response: &jsonrpc::Response) -> std::io::Result<()> {
    let text = serde_json::to_string(response).expect("a response serializes");
    output.write_all(text.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

/// What the loaded extensions contribute, as the notification clients read.
///
/// The kernel's shapes are not the wire's — `jan-klod-core` does not depend on
/// `jan-klod-protocol` — so a surface maps between them, the same way
/// [`notification_for`] maps a turn event.
fn contributions_notification(agent: &mut AgentSession) -> Notification {
    let extensions = agent
        .contributions()
        .into_iter()
        .map(|set| jan_klod_protocol::Contributions {
            extension: set.extension,
            commands: set
                .commands
                .into_iter()
                .map(|command| jan_klod_protocol::SurfaceCommand {
                    name: command.name,
                    title: command.title,
                    description: command.description,
                    arguments: command
                        .arguments
                        .into_iter()
                        .map(|argument| jan_klod_protocol::Argument {
                            name: argument.name,
                            description: argument.description,
                            required: argument.required,
                        })
                        .collect(),
                })
                .collect(),
            status_items: set
                .status_items
                .into_iter()
                .map(|item| jan_klod_protocol::StatusItem {
                    name: item.name,
                    text: item.text,
                    detail: item.detail,
                })
                .collect(),
            forms: set
                .forms
                .into_iter()
                .map(|form| jan_klod_protocol::SurfaceForm {
                    name: form.name,
                    title: form.title,
                    fields: form
                        .fields
                        .into_iter()
                        .map(|field| jan_klod_protocol::Field {
                            name: field.name,
                            label: field.label,
                            options: field.options,
                            default_value: field.default_value,
                        })
                        .collect(),
                })
                .collect(),
        })
        .collect();
    Notification::SurfaceContributions { extensions }
}

#[cfg(test)]
mod tests {
    use super::{parse, write_frame};
    use jan_klod_protocol::{jsonrpc, Command, COMMAND_METHODS};

    /// The code and message a line is refused with, or `None` if it parsed.
    fn refusal(line: &str) -> Option<(i64, jsonrpc::Id, String)> {
        match parse(line) {
            Ok(_) => None,
            Err(response) => match response.outcome {
                jsonrpc::Outcome::Error(error) => Some((error.code, response.id, error.message)),
                jsonrpc::Outcome::Result(value) => {
                    panic!("a refusal carried a result: {value}")
                }
            },
        }
    }

    #[test]
    fn a_well_formed_frame_parses_to_its_command() {
        let request =
            parse(r#"{"jsonrpc":"2.0","id":1,"method":"session/get","params":{"session":"s1"}}"#)
                .expect("parses");
        assert_eq!(request.id, jsonrpc::Id::Number(1));
        assert_eq!(
            request.command,
            Command::SessionGet {
                session: "s1".to_owned()
            }
        );
    }

    #[test]
    fn a_command_with_no_params_needs_no_params_member() {
        let request =
            parse(r#"{"jsonrpc":"2.0","id":"a","method":"session/list"}"#).expect("parses");
        assert_eq!(request.id, jsonrpc::Id::Text("a".to_owned()));
        assert_eq!(request.command, Command::SessionList);
    }

    /// Not JSON at all. The answer carries a null id because there is no id to
    /// read — which is the one case the spec singles out.
    #[test]
    fn a_line_that_is_not_json_is_a_parse_error_with_a_null_id() {
        let (code, id, _) = refusal("this is not json").expect("refused");
        assert_eq!(code, jsonrpc::PARSE_ERROR);
        assert_eq!(id, jsonrpc::Id::Null);
    }

    /// Valid JSON, wrong shape.
    #[test]
    fn json_that_is_not_a_request_object_is_an_invalid_request() {
        for line in ["[1,2,3]", "\"hello\"", "42", "null"] {
            let (code, id, _) = refusal(line).unwrap_or_else(|| panic!("{line} must be refused"));
            assert_eq!(code, jsonrpc::INVALID_REQUEST, "{line}");
            assert_eq!(id, jsonrpc::Id::Null, "{line}");
        }
    }

    /// The version member is not decoration: a frame that does not declare
    /// JSON-RPC 2.0 is not one, and its id is still answered so the client is
    /// not left waiting.
    #[test]
    fn a_frame_declaring_another_jsonrpc_version_is_refused_against_its_id() {
        let (code, id, message) =
            refusal(r#"{"jsonrpc":"1.0","id":4,"method":"session/list"}"#).expect("refused");
        assert_eq!(code, jsonrpc::INVALID_REQUEST);
        assert_eq!(id, jsonrpc::Id::Number(4));
        assert!(message.contains("2.0"), "says what it must be: {message}");
    }

    #[test]
    fn a_request_without_an_id_is_refused() {
        // A frame with no id is a JSON-RPC *notification*, and this contract has
        // no client-to-core notifications: every command is answered. Accepting
        // one would mean serving a command whose answer goes nowhere.
        let (code, id, _) =
            refusal(r#"{"jsonrpc":"2.0","method":"session/list"}"#).expect("refused");
        assert_eq!(code, jsonrpc::INVALID_REQUEST);
        assert_eq!(id, jsonrpc::Id::Null);
    }

    /// An id a client cannot correlate an answer to is refused, so the null id
    /// stays what it is documented to be: this core's answer to a frame it
    /// could not read one from.
    #[test]
    fn a_client_may_not_send_a_null_id() {
        let (code, id, message) =
            refusal(r#"{"jsonrpc":"2.0","id":null,"method":"session/list"}"#).expect("refused");
        assert_eq!(code, jsonrpc::INVALID_REQUEST);
        assert_eq!(id, jsonrpc::Id::Null);
        assert!(message.contains("null"), "says which member: {message}");
    }

    /// The distinction `COMMAND_METHODS` exists for. Both of these are one
    /// serde error; a client author needs to know which mistake they made.
    #[test]
    fn an_unknown_method_and_bad_params_are_told_apart() {
        let (code, id, message) =
            refusal(r#"{"jsonrpc":"2.0","id":1,"method":"session/destroy"}"#).expect("refused");
        assert_eq!(code, jsonrpc::METHOD_NOT_FOUND);
        assert_eq!(id, jsonrpc::Id::Number(1));
        assert!(
            message.contains("session/destroy"),
            "names the method: {message}"
        );

        let (code, id, message) =
            refusal(r#"{"jsonrpc":"2.0","id":2,"method":"session/get"}"#).expect("refused");
        assert_eq!(
            code,
            jsonrpc::INVALID_PARAMS,
            "session/get exists; its params were missing"
        );
        assert_eq!(id, jsonrpc::Id::Number(2));
        assert!(
            message.contains("session/get"),
            "names the method: {message}"
        );
    }

    /// Every method in the contract reaches a command, so none of them can be
    /// answered with `method not found` — the failure this whole diagnosis
    /// exists to avoid getting wrong.
    #[test]
    fn no_documented_method_is_reported_as_unknown() {
        for method in COMMAND_METHODS {
            let line = format!(r#"{{"jsonrpc":"2.0","id":1,"method":"{method}"}}"#);
            if let Some((code, _, message)) = refusal(&line) {
                assert_ne!(
                    code,
                    jsonrpc::METHOD_NOT_FOUND,
                    "`{method}` is in the contract but reported unknown: {message}"
                );
            }
        }
    }

    /// Every frame ends in a newline and is flushed, because the reader on the
    /// other end is splitting on newlines and blocking until it sees one.
    #[test]
    fn a_frame_is_written_as_one_flushed_line() {
        struct CountingWriter {
            bytes: Vec<u8>,
            flushes: usize,
        }
        impl std::io::Write for CountingWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.bytes.extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.flushes += 1;
                Ok(())
            }
        }

        let mut writer = CountingWriter {
            bytes: Vec::new(),
            flushes: 0,
        };
        write_frame(
            &mut writer,
            &jsonrpc::Response::result(jsonrpc::Id::Number(1), serde_json::json!({ "ok": true })),
        )
        .expect("writes");
        let text = String::from_utf8(writer.bytes).expect("utf-8");
        assert_eq!(
            text,
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n"
        );
        assert_eq!(text.matches('\n').count(), 1, "exactly one line: {text:?}");
        assert_eq!(writer.flushes, 1, "written and flushed, not just written");
    }
}
