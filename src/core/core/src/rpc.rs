//! Newline-delimited JSON-RPC 2.0 over a byte stream — the transport a client
//! gets by spawning the core rather than connecting to it.
//!
//! One frame per line, one line per frame. No port, no token, no server left
//! running: the client owns the process, and the process dies with it. This is
//! the shape editors already speak (LSP, Codex's `app-server`), which is why
//! Phase 18's MCP and ACP ports adapt *this* rather than the REST surface.
//!
//! # Generic over the streams, on purpose
//!
//! [`serve`] takes any [`BufRead`] and any [`Write`], not `stdin()`/`stdout()`.
//! The gateway passes the real ones; the tests pass pipes and buffers, so a
//! full exchange is asserted in-process with no subprocess and no model. Slice
//! 13c's WebSocket transport is the same dispatch over a different pair.
//!
//! # The handshake is required, not offered
//!
//! `protocol/hello` must be the first frame. A negotiation a client can skip
//! negotiates nothing: a client built against a version this core cannot talk
//! to would otherwise drive it happily, which is the failure the version exists
//! to prevent. An incompatible client is told what this core speaks and the
//! connection closes — there is nothing further to say to it.

use std::io::{BufRead, Write};

use jan_klod_protocol::{
    compatible, jsonrpc, Command, HelloResult, COMMAND_METHODS, PROTOCOL_VERSION,
};

use crate::serve::{self, Forked};
use crate::AgentSession;

/// Serve frames from `input`, answering on `output`, until the input ends.
///
/// Returns `Ok(())` when the client goes away (EOF) or when the handshake was
/// refused. A malformed frame is answered and the loop continues: one client
/// sending nonsense should not take the session down with it.
///
/// # Errors
/// Propagates a write failure, and a read failure other than invalid UTF-8 —
/// both mean the pipe is gone, so there is no one left to answer.
pub fn serve<R: BufRead, W: Write>(
    input: R,
    mut output: W,
    agent: &AgentSession,
) -> std::io::Result<()> {
    let mut negotiated = false;
    for line in input.lines() {
        let line = match line {
            Ok(line) => line,
            // Bytes that are not UTF-8 are this frame's problem, not the
            // stream's: the line has been consumed, so answering and carrying
            // on is both possible and kinder than hanging up.
            Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
                let refusal = refuse(
                    jsonrpc::Id::Null,
                    jsonrpc::PARSE_ERROR,
                    "a frame must be UTF-8".to_owned(),
                );
                write_frame(&mut output, &refusal)?;
                continue;
            }
            Err(err) => return Err(err),
        };
        if line.trim().is_empty() {
            continue;
        }
        match dispatch(&line, &mut negotiated, agent) {
            Served::Answer(response) => write_frame(&mut output, &response)?,
            Served::Close(response) => {
                write_frame(&mut output, &response)?;
                return Ok(());
            }
        }
    }
    Ok(())
}

/// What serving one frame decided.
enum Served {
    /// Answer it and read the next frame.
    Answer(jsonrpc::Response),
    /// Answer it and hang up. Only a refused handshake does this.
    Close(jsonrpc::Response),
}

/// Parse one line and serve whatever it turned out to be.
fn dispatch(line: &str, negotiated: &mut bool, agent: &AgentSession) -> Served {
    match parse(line) {
        Ok(request) => command(request.command, request.id, negotiated, agent),
        Err(refusal) => Served::Answer(refusal),
    }
}

/// One line to a request, or to the refusal it earns.
///
/// Separate from serving it so the frame rules — which are most of the rules —
/// can be tested without an `AgentSession`, and therefore without a booted
/// runtime, a staged `ext/` or a model.
fn parse(line: &str) -> Result<jsonrpc::Request, jsonrpc::Response> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return Err(refuse(
            jsonrpc::Id::Null,
            jsonrpc::PARSE_ERROR,
            "a frame must be one JSON object on one line".to_owned(),
        ));
    };
    // The happy path goes through the contract type, so the envelope this core
    // accepts is the one `jan-klod-protocol` describes rather than a second
    // reading of it. Only a failure needs the raw value, and only to say why.
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
/// Serde reports "no such command" and "wrong arguments for that command" as
/// one error, and JSON-RPC gives them different codes — a client author
/// debugging a typo needs to know which one they made. The method name is what
/// tells them apart, which is what [`COMMAND_METHODS`] is for.
fn diagnose(value: &serde_json::Value, err: &serde_json::Error) -> jsonrpc::Response {
    let Some(object) = value.as_object() else {
        return refuse(
            jsonrpc::Id::Null,
            jsonrpc::INVALID_REQUEST,
            "a request is a JSON object".to_owned(),
        );
    };
    // The id is read before the method, and its absence is reported as its
    // absence. Ordered the other way — as it first was here — a frame with a
    // known method and no id came back as `invalid params`, sending a client
    // to look at arguments that were fine.
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
/// Only the handshake arm and the guard below are unconditional; every other
/// arm names a command, with **no wildcard**, so a command added to the
/// protocol stops this from compiling until this transport says what it does
/// with it. (A guarded arm does not count towards exhaustiveness, which is what
/// lets the handshake gate sit in the middle of the match instead of being an
/// early return that has to repeat itself.)
fn command(
    command: Command,
    id: jsonrpc::Id,
    negotiated: &mut bool,
    agent: &AgentSession,
) -> Served {
    match command {
        Command::Hello { version } => {
            if compatible(PROTOCOL_VERSION, &version) {
                *negotiated = true;
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
        // Everything past the handshake requires the handshake.
        _ if !*negotiated => Served::Answer(refuse(
            id,
            jsonrpc::INVALID_REQUEST,
            "send `protocol/hello` first: this core does not serve a client whose \
             protocol version it has not agreed"
                .to_owned(),
        )),
        Command::SessionCreate => answer(id, serde_json::json!({ "id": serve::new_session_id() })),
        Command::SessionList => answer(id, serve::sessions_payload(agent)),
        Command::SessionGet { session } => answer(id, serve::session_payload(agent, &session)),
        Command::SessionFork { session, at_seq } => match serve::fork(agent, &session, at_seq) {
            Forked::Created(payload) => answer(id, payload),
            // The caller asked to copy events that are not there — their
            // arguments, not the core's failure.
            Forked::Empty(message) => Served::Answer(refuse(id, jsonrpc::INVALID_PARAMS, message)),
            Forked::Failed(message) => Served::Answer(refuse(id, jsonrpc::INTERNAL_ERROR, message)),
        },
        // Turns are the next step of Slice 13b. Refused rather than accepted
        // and dropped: `method not found` is the spec's code for a method that
        // exists but "is not available", and a client that is told so can fall
        // back to REST, while one whose turn silently never starts cannot.
        Command::SessionMessage { .. }
        | Command::TurnAnswer { .. }
        | Command::TurnCancel { .. }
        | Command::TurnFollowUp { .. } => Served::Answer(refuse(
            id,
            jsonrpc::METHOD_NOT_FOUND,
            "this transport does not run turns yet".to_owned(),
        )),
    }
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
