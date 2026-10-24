//! Model Context Protocol, server side: a synchronous adapter over [`crate::rpc`] framing, with no SDK.

use std::io::{BufRead, Write};

use jk_protocol::jsonrpc;

use jk_agent::{AgentSession, HeadlessDriver};

/// The MCP spec revision this server implements, a date negotiated separately from `PROTOCOL_VERSION`.
pub const MCP_VERSION: &str = "2025-11-25";

const SERVER_NAME: &str = "jan-klod";

const DEFAULT_SESSION: &str = "mcp";

/// `id` is optional because notifications have none.
#[derive(Debug, serde::Deserialize)]
struct Frame {
    jsonrpc: String,
    #[serde(default)]
    id: Option<jsonrpc::Id>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

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
/// Any I/O failure on `output`. A malformed frame is answered, not returned.
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

/// A failing tool reports in band (`isError: true`), since a JSON-RPC error would make every permission refusal read as a broken server.
fn call_ask(agent: &mut AgentSession, params: &serde_json::Value) -> serde_json::Value {
    let arguments = params.get("arguments").unwrap_or(&serde_json::Value::Null);
    let Some(question) = arguments
        .get("question")
        .and_then(serde_json::Value::as_str)
    else {
        return content("`ask` needs a `question` string", true);
    };
    let session = arguments
        .get("session")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(DEFAULT_SESSION);

    let mut driver = HeadlessDriver;
    match agent.run_with_driver(&mut driver, session, question) {
        jk_agent::conductor::RunResult::Answered { text, .. } => content(&text, false),
        jk_agent::conductor::RunResult::Failed(message) => content(&message, true),
    }
}

/// Returned as pretty-printed JSON text: `structuredContent` is not universally supported.
fn call_sessions(agent: &AgentSession, params: &serde_json::Value, one: bool) -> serde_json::Value {
    let payload = if one {
        let arguments = params.get("arguments").unwrap_or(&serde_json::Value::Null);
        let Some(id) = arguments.get("session").and_then(serde_json::Value::as_str) else {
            return content("`session_get` needs a `session` string", true);
        };
        jk_agent::session::session_payload(agent, id)
    } else {
        jk_agent::session::sessions_payload(agent)
    };
    match serde_json::to_string_pretty(&payload) {
        Ok(text) => content(&text, false),
        Err(err) => content(
            &format!("the session payload could not be rendered: {err}"),
            true,
        ),
    }
}

fn content(text: &str, is_error: bool) -> serde_json::Value {
    serde_json::json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    })
}

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

enum Asked {
    Silent,
    Answer(jsonrpc::Response),
    Ask {
        id: jsonrpc::Id,
        params: serde_json::Value,
    },
    Sessions {
        id: jsonrpc::Id,
        params: serde_json::Value,
        /// `true` for `session_get`, `false` for `session_list`.
        one: bool,
    },
}

fn classify(line: &str) -> Asked {
    let frame: Frame = match serde_json::from_str(line) {
        Ok(frame) => frame,
        Err(err) => {
            return Asked::Answer(refuse(
                jsonrpc::Id::Null,
                jsonrpc::PARSE_ERROR,
                format!("a frame must be one JSON-RPC object on one line: {err}"),
            ));
        }
    };

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

/// The client's `protocolVersion` is read but not honoured: this server speaks one revision and says which.
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
        "instructions": format!(
            "jan-klod speaks MCP {MCP_VERSION}; the client asked for {asked}. \
             Tools: ask, session_list, session_get."
        ),
    })
}

fn refuse(id: jsonrpc::Id, code: i64, message: String) -> jsonrpc::Response {
    jsonrpc::Response::error(id, jsonrpc::Error::new(code, message))
}

/// Writes one frame and flushes it: pipe stdout is block-buffered.
fn write_frame<W: Write>(output: &mut W, response: &jsonrpc::Response) -> std::io::Result<()> {
    let text = serde_json::to_string(response).expect("a response serializes");
    output.write_all(text.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::{classify, content, tools, Asked, MCP_VERSION};
    use jk_protocol::jsonrpc;

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

    #[test]
    fn tools_list_works_without_params() {
        let value = call(r#"{"jsonrpc":"2.0","id":3,"method":"tools/list"}"#);
        assert_eq!(value["result"]["tools"].as_array().map(Vec::len), Some(3));
    }

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
