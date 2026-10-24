//! Newline-delimited JSON-RPC 2.0 over a byte stream for spawned clients; `protocol/hello` must come first so the version check cannot be skipped.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{BufRead, Write};
use std::rc::Rc;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Instant;

use jk_protocol::{
    compatible, jsonrpc, Command, HelloResult, Notification, SurfaceInvokeResult, COMMAND_METHODS,
    PROTOCOL_VERSION,
};

use jk_agent::conductor::RunResult;

use jk_agent::session::{self, Forked};
use jk_agent::AgentSession;
use jk_protocol::turn::{Driver, UserPrompt};
use jk_protocol::turn::{Event, EventSink, Flow};

/// Serves frames from `input`, answering on `output`, until EOF or a refused handshake.
///
/// # Errors
/// Write failures: the pipe is gone.
pub fn serve<R: BufRead + Send + 'static, W: Write>(
    input: R,
    output: W,
    agent: &mut AgentSession,
) -> std::io::Result<()> {
    let (handover, incoming) = mpsc::channel();
    std::thread::spawn(move || {
        for line in input.lines() {
            let handed = match line {
                Ok(line) => handover.send(Incoming::Line(line)),
                Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                    handover.send(Incoming::NotUtf8)
                }
                Err(_) => break,
            };
            if handed.is_err() {
                break;
            }
        }
    });

    let wire = Wire {
        writer: Rc::new(RefCell::new(output)),
        incoming: &incoming,
    };
    let mut negotiated = false;
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
    Ok(())
}

pub(crate) enum Incoming {
    Line(String),
    NotUtf8,
}

pub(crate) struct Wire<'a, W: Write> {
    /// Shared by the turn's sink and driver; `Rc<RefCell<_>>` since one thread writes.
    writer: Rc<RefCell<W>>,
    incoming: &'a Receiver<Incoming>,
}

impl<'a, W: Write> Wire<'a, W> {
    pub(crate) const fn new(writer: Rc<RefCell<W>>, incoming: &'a Receiver<Incoming>) -> Self {
        Self { writer, incoming }
    }
}

impl<W: Write> Wire<'_, W> {
    pub(crate) fn write(&self, response: &jsonrpc::Response) -> std::io::Result<()> {
        write_frame(&mut *self.writer.borrow_mut(), response)
    }
}

pub(crate) enum Served {
    Answer(jsonrpc::Response),
    /// Answer it and hang up; only a refused handshake does this.
    Close(jsonrpc::Response),
}

/// Beside the command enum so a new variant is a compile error here.
#[must_use]
pub(crate) fn session_of(command: &Command) -> Option<&str> {
    match command {
        Command::SessionGet { session }
        | Command::SessionFork { session, .. }
        | Command::SessionMessage { session, .. }
        | Command::TurnAnswer { session, .. }
        | Command::TurnCancel { session }
        | Command::TurnFollowUp { session, .. } => Some(session),
        Command::Hello { .. }
        | Command::SessionCreate
        | Command::SessionList
        | Command::SurfaceInvoke { .. } => None,
    }
}

#[must_use]
pub(crate) fn not_text() -> String {
    let response = refuse(
        jsonrpc::Id::Null,
        jsonrpc::PARSE_ERROR,
        "a frame must be text, one JSON object".to_owned(),
    );
    serde_json::to_string(&response).unwrap_or_else(|_| String::from("{}"))
}

pub(crate) fn parse(line: &str) -> Result<jsonrpc::Request, jsonrpc::Response> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Err(refuse(
            jsonrpc::Id::Null,
            jsonrpc::PARSE_ERROR,
            "a frame must be one JSON object on one line".to_owned(),
        ));
    };
    match serde_json::from_str::<jsonrpc::Request>(line) {
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

/// Separates "no such command" from "wrong arguments" via [`COMMAND_METHODS`]: serde gives one code, JSON-RPC two.
fn diagnose(value: &serde_json::Value, err: &serde_json::Error) -> jsonrpc::Response {
    let Some(object) = value.as_object() else {
        return refuse(
            jsonrpc::Id::Null,
            jsonrpc::INVALID_REQUEST,
            "a request is a JSON object".to_owned(),
        );
    };
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

/// Every arm names a command (no wildcard), so new protocol commands fail to compile until handled.
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
            Forked::Empty(message) => Served::Answer(refuse(id, jsonrpc::INVALID_PARAMS, message)),
            Forked::Failed(message) => Served::Answer(refuse(id, jsonrpc::INTERNAL_ERROR, message)),
        },
        Command::SessionMessage { session, message } => turn(&session, &message, id, agent, wire),
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
            let arguments: Vec<jk_protocol::contributions::ArgumentValue> = arguments
                .into_iter()
                .map(|argument| jk_protocol::contributions::ArgumentValue {
                    name: argument.name,
                    value: argument.value,
                })
                .collect();
            match agent.invoke_contribution(&extension, &name, &arguments) {
                Ok(outcome) => {
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
                Err(error @ jk_protocol::contributions::InvokeError::Unknown) => {
                    Served::Answer(refuse(id, jsonrpc::METHOD_NOT_FOUND, error.to_string()))
                }
                Err(error @ jk_protocol::contributions::InvokeError::InvalidArguments) => {
                    Served::Answer(refuse(id, jsonrpc::INVALID_PARAMS, error.to_string()))
                }
                Err(error @ jk_protocol::contributions::InvokeError::Failed(_)) => {
                    Served::Answer(refuse(id, jsonrpc::INTERNAL_ERROR, error.to_string()))
                }
            }
        }
    }
}

/// Deliberately duplicates `done`: clients can await the response or render the stream, not both.
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
        RunResult::Failed(reason) => Served::Answer(refuse(id, jsonrpc::INTERNAL_ERROR, reason)),
    }
}

struct TurnState {
    /// A `turn/cancel` naming another session is refused: cancelling the wrong turn silently would be worse.
    session: String,
    cancelled: Cell<bool>,
    follow_ups: RefCell<VecDeque<String>>,
    /// An answer when nothing asked is refused, not stashed (REST: `409`): a stashed one would approve the next question.
    asking: Cell<bool>,
}

struct RpcSink<'a, W: Write> {
    writer: Rc<RefCell<W>>,
    incoming: &'a Receiver<Incoming>,
    state: Rc<TurnState>,
}

impl<W: Write> EventSink for RpcSink<'_, W> {
    fn emit(&mut self, event: &Event) -> Flow {
        if write_notification(&mut *self.writer.borrow_mut(), &notification_for(event)).is_err() {
            return Flow::Stop;
        }
        serve_queued(self.incoming, &self.state, &self.writer);
        if self.state.cancelled.get() {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }
}

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
            return prompt.default_answer.clone();
        }
        self.wait_for_answer()
            .unwrap_or_else(|| prompt.default_answer.clone())
    }

    fn follow_up(&mut self) -> Option<String> {
        serve_queued(self.incoming, &self.state, &self.writer);
        self.state.follow_ups.borrow_mut().pop_front()
    }
}

impl<W: Write> RpcDriver<'_, W> {
    /// No heartbeat probe is needed: the pipe closing closes the channel, and `recv_timeout` reports `Disconnected`.
    fn wait_for_answer(&self) -> Option<String> {
        let deadline = Instant::now() + session::answer_timeout();
        self.state.asking.set(true);
        let answer = self.wait_until(deadline);
        self.state.asking.set(false);
        answer
    }

    /// The wait itself, so `asking` is cleared on every path out.
    fn wait_until(&self, deadline: Instant) -> Option<String> {
        loop {
            let remaining = deadline.checked_duration_since(Instant::now())?;
            match self.incoming.recv_timeout(remaining) {
                Ok(frame) => {
                    if let Some(answer) = serve_frame(frame, &self.state, &self.writer) {
                        return Some(answer);
                    }
                    if self.state.cancelled.get() {
                        return None;
                    }
                }
                Err(RecvTimeoutError::Timeout | RecvTimeoutError::Disconnected) => return None,
            }
        }
    }
}

fn serve_queued<W: Write>(
    incoming: &Receiver<Incoming>,
    state: &TurnState,
    writer: &Rc<RefCell<W>>,
) {
    while let Ok(frame) = incoming.try_recv() {
        drop(serve_frame(frame, state, writer));
    }
}

/// Returns the text of a `turn/answer` for this session; everything else is answered in place, so a client told "busy" can retry.
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

/// The notification each turn event becomes; exhaustive, so a new `Event` variant fails to compile until handled.
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

fn answer(id: jsonrpc::Id, result: serde_json::Value) -> Served {
    Served::Answer(jsonrpc::Response::result(id, result))
}

fn refuse(id: jsonrpc::Id, code: i64, message: String) -> jsonrpc::Response {
    jsonrpc::Response::error(id, jsonrpc::Error::new(code, message))
}

/// Writes one frame and flushes it: pipe stdout is block-buffered, so a client would otherwise hang.
fn write_frame<W: Write>(output: &mut W, response: &jsonrpc::Response) -> std::io::Result<()> {
    let text = serde_json::to_string(response).expect("a response serializes");
    output.write_all(text.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

/// Maps kernel shapes to wire shapes, as `jk-agent` does not depend on `jk-protocol`.
fn contributions_notification(agent: &mut AgentSession) -> Notification {
    let extensions = agent
        .contributions()
        .into_iter()
        .map(|set| jk_protocol::Contributions {
            extension: set.extension,
            commands: set
                .commands
                .into_iter()
                .map(|command| jk_protocol::SurfaceCommand {
                    name: command.name,
                    title: command.title,
                    description: command.description,
                    arguments: command
                        .arguments
                        .into_iter()
                        .map(|argument| jk_protocol::Argument {
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
                .map(|item| jk_protocol::StatusItem {
                    name: item.name,
                    text: item.text,
                    detail: item.detail,
                })
                .collect(),
            forms: set
                .forms
                .into_iter()
                .map(|form| jk_protocol::SurfaceForm {
                    name: form.name,
                    title: form.title,
                    fields: form
                        .fields
                        .into_iter()
                        .map(|field| jk_protocol::Field {
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
    use jk_protocol::{jsonrpc, Command, COMMAND_METHODS};

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

    #[test]
    fn a_line_that_is_not_json_is_a_parse_error_with_a_null_id() {
        let (code, id, _) = refusal("this is not json").expect("refused");
        assert_eq!(code, jsonrpc::PARSE_ERROR);
        assert_eq!(id, jsonrpc::Id::Null);
    }

    #[test]
    fn json_that_is_not_a_request_object_is_an_invalid_request() {
        for line in ["[1,2,3]", "\"hello\"", "42", "null"] {
            let (code, id, _) = refusal(line).unwrap_or_else(|| panic!("{line} must be refused"));
            assert_eq!(code, jsonrpc::INVALID_REQUEST, "{line}");
            assert_eq!(id, jsonrpc::Id::Null, "{line}");
        }
    }

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
        let (code, id, _) =
            refusal(r#"{"jsonrpc":"2.0","method":"session/list"}"#).expect("refused");
        assert_eq!(code, jsonrpc::INVALID_REQUEST);
        assert_eq!(id, jsonrpc::Id::Null);
    }

    #[test]
    fn a_client_may_not_send_a_null_id() {
        let (code, id, message) =
            refusal(r#"{"jsonrpc":"2.0","id":null,"method":"session/list"}"#).expect("refused");
        assert_eq!(code, jsonrpc::INVALID_REQUEST);
        assert_eq!(id, jsonrpc::Id::Null);
        assert!(message.contains("null"), "says which member: {message}");
    }

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
