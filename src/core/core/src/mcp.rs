//! Model Context Protocol, server side — the outbound half of the ecosystem port.
//!
//! `registry-mcp` consumes MCP servers; this makes the core one, so Claude Code,
//! Codex, Goose and editors can call it the way they call anything else.
//!
//! # Why there is no SDK here
//!
//! MCP's stdio transport is the framing this repository already speaks. The
//! spec: messages "delimited by newlines and must not contain embedded
//! newlines", stdout for frames, stderr for logging — which is
//! [`crate::rpc`] exactly. So this is a method-name-and-payload adapter, not a
//! transport.
//!
//! `rmcp`, the official Rust SDK, is async on tokio. The core is deliberately
//! synchronous: the agent session is `!Send` and lives on one thread, and
//! `tiny_http` was chosen over `axum` for the same reason. Adopting an async
//! SDK would be an architectural change dressed as a convenience, so the
//! envelope is reused instead and this module costs no new dependency.
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

/// The MCP spec revision this server implements.
///
/// A date, not a semver — and negotiated separately from this repository's own
/// `PROTOCOL_VERSION`. It moves: check the spec rather than trusting this
/// constant to still be current.
pub const MCP_VERSION: &str = "2025-11-25";

/// What this server calls itself in `initialize`.
const SERVER_NAME: &str = "jan-klod";

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
/// Discovery only for now: `initialize`, `notifications/initialized` and
/// `tools/list`. `tools/call` answers `METHOD_NOT_FOUND` with a message saying
/// so, which is a clearer thing for a client author to read than a tool that
/// half-runs — and the tools are advertised because that is what makes the
/// discovery surface testable at all.
///
/// # Errors
/// Any I/O failure on `output`. A malformed *frame* is answered, not returned:
/// the loop keeps serving, because one bad line from a client is not a reason
/// to hang up on it.
pub fn serve<R: BufRead, W: Write>(input: R, output: &mut W) -> std::io::Result<()> {
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = answer(&line) {
            write_frame(output, &response)?;
        }
    }
    Ok(())
}

/// One line to the response it earns, or `None` for a notification.
///
/// Split out so the frame rules can be tested without a runtime — there is no
/// `AgentSession` in this box at all, which is the other reason `tools/call`
/// waits.
fn answer(line: &str) -> Option<jsonrpc::Response> {
    let frame: Frame = match serde_json::from_str(line) {
        Ok(frame) => frame,
        Err(err) => {
            // No id could be read, so the answer carries a null one. The spec
            // allows exactly this for a frame that could not be parsed.
            return Some(refuse(
                jsonrpc::Id::Null,
                jsonrpc::PARSE_ERROR,
                format!("a frame must be one JSON-RPC object on one line: {err}"),
            ));
        }
    };

    // A notification has no id and earns no answer — including a malformed one,
    // since there is nothing to correlate a complaint with.
    let id = frame.id?;

    if frame.jsonrpc != jsonrpc::VERSION {
        return Some(refuse(
            id,
            jsonrpc::INVALID_REQUEST,
            format!(
                "`jsonrpc` must be \"{}\", not \"{}\"",
                jsonrpc::VERSION,
                frame.jsonrpc
            ),
        ));
    }

    Some(match frame.method.as_str() {
        "initialize" => jsonrpc::Response::result(id, initialized(&frame.params)),
        "tools/list" => jsonrpc::Response::result(id, serde_json::json!({ "tools": tools() })),
        "tools/call" => refuse(
            id,
            jsonrpc::METHOD_NOT_FOUND,
            "`tools/call` is not served yet; `initialize` and `tools/list` are".to_owned(),
        ),
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
    use super::{answer, tools, MCP_VERSION};
    use jan_klod_protocol::jsonrpc;

    fn call(line: &str) -> serde_json::Value {
        let response = answer(line).expect("a request earns an answer");
        serde_json::to_value(response).expect("it serializes")
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
            answer(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).is_none(),
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

    /// Advertised but not yet served, and the message says which — a client
    /// author reading `METHOD_NOT_FOUND` for a tool they can see in
    /// `tools/list` would otherwise assume a typo.
    #[test]
    fn tools_call_says_it_is_not_served_yet() {
        let value =
            call(r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"ask"}}"#);
        assert_eq!(value["error"]["code"], jsonrpc::METHOD_NOT_FOUND);
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("not served yet"),
            "{value}"
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
