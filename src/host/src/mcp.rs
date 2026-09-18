//! Model Context Protocol, server side — the outbound half of the ecosystem port.
//!
//! `registry-mcp` consumes MCP servers; this makes the core one, so Claude Code,
//! Codex, Goose and editors can call it the way they call anything else.
//!
//! # Why there is no SDK here
//!
//! MCP's stdio transport is the framing this repository already speaks. The spec:
//! messages "delimited by newlines and must not contain embedded newlines",
//! stdout for frames, stderr for logging — which is [`crate::rpc`] exactly.
//! So this is a method-name-and-payload adapter, not a transport.
//!
//! `rmcp`, the official Rust SDK, is async on tokio. This module stays
//! synchronous anyway: the agent session is `!Send` and lives on one
//! thread, and reaching it means going through the job queue
//! (`session_thread`) rather than holding it. Adopting an async SDK would
//! be an architectural change dressed as convenience, so the envelope is
//! reused and this module costs no new dependency.
//!
//! The sentence that used to be here said `tiny_http` was chosen over
//! `axum` for that reason. The REST surface is `axum` as of #224, and the
//! reason turned out to be about who owns the session rather than about
//! which library serves — the other four claims of the same shape are
//! #227's.
//!
//! # What is reused, and what could not be
//!
//! [`jsonrpc::Response`], [`jsonrpc::Id`], [`jsonrpc::Error`] and the error
//! codes are reused verbatim, as is the flush discipline a pipe demands.
//!
//! [`jsonrpc::Request`] could **not** be: it flattens into this repository's
//! own `Command` enum, so it accepts `turn/start` and not `tools/call`. An MCP
//! frame therefore has its own shape here — a method name and free-form
//! `params` — and the two version schemes stay separate: [`MCP_VERSION`] is
//! what an editor speaks, `PROTOCOL_VERSION` is what `jan-klod` speaks.

use std::io::{BufRead, Write};

use jan_klod_protocol::jsonrpc;

use jan_klod_core::{AgentSession, HeadlessDriver};

/// The MCP spec revision this server implements.
///
/// A date, not a semver — and negotiated separately from this repository's own
/// `PROTOCOL_VERSION`. It moves: check the spec rather than trusting this
/// constant to still be current.
pub const MCP_VERSION: &str = "2025-11-25";

/// What this server calls itself in `initialize`.
const SERVER_NAME: &str = "jan-klod";

/// The session an `ask` uses when the caller names none.
const DEFAULT_SESSION: &str = "mcp";

/// One MCP request or notification.
///
/// `id` is optional because a notification has none — `notifications/initialized`
/// arrives right after the handshake, and a server that demanded an id would
/// reject the first thing every client sends after `initialize`.
#[derive(Debug, serde::Deserialize)]
struct Frame {
    jsonrpc: String,
    #[serde(default)]
    id: Option<jsonrpc::Id>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// The tools this server exposes, with their argument schemas.
///
/// Built rather than `const` because the schemas are JSON; the list is fixed.
fn tools() -> serde_json::Value {
    serde_json::json!([
        {
            "name": "ask",
            "description": "Run one agent turn and return the final answer. \
                            Reads and searches freely; a turn needing a write or \
                            a command is refused, because an MCP client cannot \
                            answer a confirmation prompt.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "What to ask." },
                    "session": {
                        "type": "string",
                        "description": "Session id to continue. A new one is made when absent."
                    }
                },
                "required": ["question"],
                "additionalProperties": false
            }
        },
        {
            "name": "session_list",
            "description": "Every session this core holds, most recent first.",
            "inputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        },
        {
            "name": "session_get",
            "description": "One session's transcript.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "session": { "type": "string", "description": "The session id." }
                },
                "required": ["session"],
                "additionalProperties": false
            }
        }
    ])
}

/// Serve MCP on `input`/`output` until the client hangs up.
///
/// # Errors
/// Any I/O failure on `output`. A malformed *frame* is answered, not returned:
/// the loop serves on because one bad line from a client isn't a reason to
/// hang up on it.
pub fn serve<R: BufRead, W: Write>(
    input: R,
    output: &mut W,
    agent: &mut AgentSession,
) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = answer(&line, agent) {
            write_frame(output, &response)?;
        }
    }
    Ok(())
}

/// Run one turn and render it as MCP tool content.
///
/// # `isError` is a field on a *successful* result
///
/// This is the shape MCP chose, opposite this repository's [`jsonrpc::Outcome`],
/// which makes result and error mutually exclusive so "both" and "neither" are
/// unrepresentable. Here a failing tool reports it **in band**: the JSON-RPC
/// response is a success carrying `isError: true`.
///
/// Mapping a refused turn onto a JSON-RPC error would make every permission
/// refusal read to an editor as a broken server — the failure that looks fine
/// in passing tests and wrong in use. A refusal is an answer, not a transport
/// fault.
fn call_ask(agent: &mut AgentSession, params: &serde_json::Value) -> serde_json::Value {
    let arguments = params.get("arguments").unwrap_or(&serde_json::Value::Null);
    let Some(question) = arguments
        .get("question")
        .and_then(serde_json::Value::as_str)
    else {
        return content("`ask` needs a `question` string", true);
    };
    // A named session continues; an absent one gets this server's own, so two
    // calls in a row share a transcript rather than starting fresh each time.
    let session = arguments
        .get("session")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(DEFAULT_SESSION);

    // `HeadlessDriver`, always: see its docs. Prompting would read a protocol
    // frame as an answer, editors couldn't answer anyway.
    let mut driver = HeadlessDriver;
    match agent.run_with_driver(&mut driver, session, question) {
        jan_klod_core::conductor::RunResult::Answered { text, .. } => content(&text, false),
        // The message is written for a person; an enum's Debug is not a
        // diagnosis, and this one reaches a model.
        jan_klod_core::conductor::RunResult::Failed(message) => content(&message, true),
    }
}

/// The two read tools, over the payloads the REST surface already builds.
///
/// `session::sessions_payload` and `session::session_payload` are reused rather
/// than re-derived: a second reader of the same transcripts would be a second
/// answer to "what sessions are there", and they would drift.
///
/// The payload is returned as **text**, pretty-printed JSON. Every MCP client
/// can render a text block; `structuredContent` is newer and not universally
/// supported, and a model reads JSON perfectly well.
fn call_sessions(agent: &AgentSession, params: &serde_json::Value, one: bool) -> serde_json::Value {
    let payload = if one {
        let arguments = params.get("arguments").unwrap_or(&serde_json::Value::Null);
        let Some(id) = arguments.get("session").and_then(serde_json::Value::as_str) else {
            return content("`session_get` needs a `session` string", true);
        };
        jan_klod_core::session::session_payload(agent, id)
    } else {
        jan_klod_core::session::sessions_payload(agent)
    };
    match serde_json::to_string_pretty(&payload) {
        Ok(text) => content(&text, false),
        Err(err) => content(
            &format!("the session payload could not be rendered: {err}"),
            true,
        ),
    }
}

/// A `tools/call` result: one text block, and whether it went wrong.
fn content(text: &str, is_error: bool) -> serde_json::Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

/// One line to the response it earns, or `None` for a notification.
fn answer(line: &str, agent: &mut AgentSession) -> Option<jsonrpc::Response> {
    match classify(line) {
        Asked::Silent => None,
        Asked::Answer(response) => Some(response),
        Asked::Ask { id, params } => Some(jsonrpc::Response::result(id, call_ask(agent, &params))),
        Asked::Sessions { id, params, one } => Some(jsonrpc::Response::result(
            id,
            call_sessions(agent, &params, one),
        )),
    }
}

/// What a line asks for, decided **without** a session.
///
/// Everything except running a turn is settled here, which is what keeps the
/// frame rules — most of the rules — testable with no booted runtime, no
/// staged `ext/` and no model. [`crate::rpc`] splits its `parse` out for the
/// same reason.
enum Asked {
    /// A notification: nothing to answer.
    Silent,
    /// Answerable without running anything.
    Answer(jsonrpc::Response),
    /// `tools/call ask`, which needs a turn.
    Ask {
        /// The request to answer.
        id: jsonrpc::Id,
        /// The `tools/call` params, arguments included.
        params: serde_json::Value,
    },
    /// `tools/call session_list` or `session_get`, which need the session store
    /// but run nothing.
    Sessions {
        /// The request to answer.
        id: jsonrpc::Id,
        /// The `tools/call` params, arguments included.
        params: serde_json::Value,
        /// `true` for `session_get`, `false` for `session_list`.
        one: bool,
    },
}

fn classify(line: &str) -> Asked {
    let frame: Frame = match serde_json::from_str(line) {
        Ok(frame) => frame,
        Err(err) => {
            // No id could be read, so the answer carries a null one. The spec
            // allows exactly this for a frame that could not be parsed.
            return Asked::Answer(refuse(
                jsonrpc::Id::Null,
                jsonrpc::PARSE_ERROR,
                format!("a frame must be one JSON-RPC object on one line: {err}"),
            ));
        }
    };

    // A notification has no id and earns no answer — including a malformed one,
    // since there is nothing to correlate a complaint with.
    let Some(id) = frame.id else {
        return Asked::Silent;
    };

    if frame.jsonrpc != jsonrpc::VERSION {
        return Asked::Answer(refuse(
            id,
            jsonrpc::INVALID_REQUEST,
            format!(
                "`jsonrpc` must be \"{}\", not \"{}\"",
                jsonrpc::VERSION,
                frame.jsonrpc
            ),
        ));
    }

    Asked::Answer(match frame.method.as_str() {
        "initialize" => jsonrpc::Response::result(id, initialized(&frame.params)),
        "tools/list" => jsonrpc::Response::result(id, serde_json::json!({ "tools": tools() })),
        "tools/call" => match frame.params.get("name").and_then(serde_json::Value::as_str) {
            Some("ask") => {
                return Asked::Ask {
                    id,
                    params: frame.params,
                }
            }
            Some(name @ ("session_list" | "session_get")) => {
                return Asked::Sessions {
                    id,
                    one: name == "session_get",
                    params: frame.params,
                }
            }
            // A tool that does not exist *is* a protocol error — the client
            // asked for something `tools/list` never offered — unlike a tool
            // that ran and failed, which is `isError`.
            Some(other) => refuse(
                id,
                jsonrpc::METHOD_NOT_FOUND,
                format!("no such tool: {other}"),
            ),
            None => refuse(
                id,
                jsonrpc::INVALID_PARAMS,
                "`tools/call` needs a `name`".to_owned(),
            ),
        },
        other => refuse(
            id,
            jsonrpc::METHOD_NOT_FOUND,
            format!("no such method: {other}"),
        ),
    })
}

/// The `initialize` result.
///
/// The client's requested `protocolVersion` is read but not honoured: this
/// server speaks one revision and says which, which the spec permits — a client
/// that cannot live with the answer disconnects. Pretending to speak a version
/// in order to agree would be worse than disagreeing clearly.
fn initialized(params: &serde_json::Value) -> serde_json::Value {
    let asked = params
        .get("protocolVersion")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unstated");
    serde_json::json!({
        "protocolVersion": MCP_VERSION,
        "capabilities": { "tools": {} },
        "serverInfo": {
            "name": SERVER_NAME,
            "version": env!("CARGO_PKG_VERSION"),
        },
        // Not part of the spec's required shape, and useful: a client whose
        // version differs can see that it was read rather than ignored.
        "instructions": format!(
            "jan-klod speaks MCP {MCP_VERSION}; the client asked for {asked}. \
             Tools: ask, session_list, session_get."
        ),
    })
}

/// A failed answer.
fn refuse(id: jsonrpc::Id, code: i64, message: String) -> jsonrpc::Response {
    jsonrpc::Response::error(id, jsonrpc::Error::new(code, message))
}

/// Write one frame and **flush it**, for the reason [`crate::rpc`] documents:
/// stdout to a pipe is block-buffered, so without the flush a client waits for
/// an answer sitting in this process's buffer.
fn write_frame<W: Write>(output: &mut W, response: &jsonrpc::Response) -> std::io::Result<()> {
    let text = serde_json::to_string(response).expect("a response serializes");
    output.write_all(text.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::{classify, content, tools, Asked, MCP_VERSION};
    use jan_klod_protocol::jsonrpc;

    /// Classify a line and render the response it earns.
    ///
    /// Through `classify` rather than `answer`, so the frame rules are tested
    /// with no booted runtime and no model — the reason the two are separate.
    fn call(line: &str) -> serde_json::Value {
        match classify(line) {
            Asked::Answer(response) => serde_json::to_value(response).expect("it serializes"),
            Asked::Silent => panic!("{line} earns an answer"),
            Asked::Ask { .. } | Asked::Sessions { .. } => {
                panic!("{line} needs a session; test it through the harness")
            }
        }
    }

    #[test]
    fn initialize_answers_with_the_version_this_server_speaks() {
        let value = call(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05"}}"#,
        );
        assert_eq!(value["result"]["protocolVersion"], MCP_VERSION);
        assert_eq!(value["result"]["serverInfo"]["name"], "jan-klod");
        assert!(
            value["result"]["capabilities"]["tools"].is_object(),
            "a tools capability is advertised: {value}"
        );
        assert!(
            value["result"]["instructions"]
                .as_str()
                .unwrap_or_default()
                .contains("2024-11-05"),
            "the version the client asked for is echoed, so a mismatch is visible: {value}"
        );
    }

    #[test]
    fn tools_list_offers_three_tools_each_with_a_schema() {
        let value = call(r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#);
        let listed = value["result"]["tools"]
            .as_array()
            .expect("tools is an array")
            .clone();
        let names: Vec<&str> = listed
            .iter()
            .map(|t| t["name"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(names, ["ask", "session_list", "session_get"]);
        for tool in &listed {
            assert_eq!(
                tool["inputSchema"]["type"], "object",
                "every tool carries an object schema: {tool}"
            );
            assert!(
                !tool["description"].as_str().unwrap_or_default().is_empty(),
                "and a description, which is what a model reads: {tool}"
            );
        }
    }

    /// `params` is optional pagination on `tools/list`, so an absent one must
    /// not be a parse error — a client that omits it is within the spec.
    #[test]
    fn tools_list_works_without_params() {
        let value = call(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
        assert_eq!(value["result"]["tools"].as_array().map(Vec::len), Some(3));
    }

    /// The frame every client sends straight after `initialize`. It has no id,
    /// so it earns no answer — and a server that demanded one would reject it.
    #[test]
    fn an_initialized_notification_is_accepted_in_silence() {
        assert!(
            matches!(
                classify(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#),
                Asked::Silent
            ),
            "a notification gets no response"
        );
    }

    #[test]
    fn a_wrong_envelope_version_is_refused() {
        let value = call(r#"{"jsonrpc":"1.0","id":4,"method":"tools/list"}"#);
        assert_eq!(value["error"]["code"], jsonrpc::INVALID_REQUEST);
    }

    #[test]
    fn an_unreadable_frame_is_answered_with_a_null_id() {
        let value = call("not json at all");
        assert_eq!(value["error"]["code"], jsonrpc::PARSE_ERROR);
        assert!(value["id"].is_null(), "{value}");
    }

    #[test]
    fn the_read_tools_are_routed_to_the_session_store() {
        let list =
            r#"{"jsonrpc":"2.0","id":8,"method":"tools/call","params":{"name":"session_list"}}"#;
        assert!(
            matches!(classify(list), Asked::Sessions { one: false, .. }),
            "session_list lists"
        );
        let get = r#"{"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"session_get","arguments":{"session":"x"}}}"#;
        assert!(
            matches!(classify(get), Asked::Sessions { one: true, .. }),
            "session_get fetches one"
        );
    }

    #[test]
    fn tools_call_ask_is_routed_to_a_turn() {
        let line = r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ask","arguments":{"question":"hi"}}}"#;
        assert!(
            matches!(classify(line), Asked::Ask { .. }),
            "`ask` needs a session, so it is classified rather than answered"
        );
    }

    /// A tool `tools/list` never offered is a *protocol* error, unlike a tool
    /// that ran and failed — which is `isError`. Conflating the two would tell
    /// a client its request was malformed when the tool simply refused.
    #[test]
    fn an_unknown_tool_is_a_protocol_error_not_an_is_error() {
        let value =
            call(r#"{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"nope"}}"#);
        assert_eq!(value["error"]["code"], jsonrpc::METHOD_NOT_FOUND);
    }

    #[test]
    fn tools_call_without_a_name_is_invalid_params() {
        let value = call(r#"{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{}}"#);
        assert_eq!(value["error"]["code"], jsonrpc::INVALID_PARAMS);
    }

    /// The shape MCP chose, pinned: a failed tool is a **successful** response
    /// carrying `isError: true`. Mapping it onto a JSON-RPC error would make
    /// every permission refusal read to an editor as a broken server.
    #[test]
    fn a_failure_is_content_with_is_error_rather_than_a_jsonrpc_error() {
        let ok = content("the answer", false);
        assert_eq!(ok["isError"], serde_json::Value::Bool(false));
        assert_eq!(ok["content"][0]["type"], "text");
        assert_eq!(ok["content"][0]["text"], "the answer");

        let bad = content("refused: a write needs confirmation", true);
        assert_eq!(bad["isError"], serde_json::Value::Bool(true));
        assert_eq!(
            bad["content"][0]["text"], "refused: a write needs confirmation",
            "the reason reaches the model, which is the only way it can adapt"
        );
        assert!(
            bad.get("error").is_none(),
            "and it is not a JSON-RPC error: {bad}"
        );
    }

    /// The schemas are `additionalProperties: false`, so a client sending an
    /// unknown argument is told rather than having it silently dropped.
    #[test]
    fn every_schema_is_closed() {
        for tool in tools().as_array().expect("an array") {
            assert_eq!(
                tool["inputSchema"]["additionalProperties"],
                serde_json::Value::Bool(false),
                "{tool}"
            );
        }
    }
}
