//! Agent Client Protocol, agent side — the editor-facing ecosystem port.
//!
//! ACP is Zed's editor↔agent standard: implement the agent half once and any
//! ACP editor connects without a bespoke plugin.
//!
//! # How this differs from [`crate::mcp`], which is not obvious
//!
//! Both are newline-delimited JSON-RPC 2.0 on stdio, so the framing is shared
//! and neither needs an SDK. Two things are not shared:
//!
//! **The version is an integer, not a date.** ACP's `protocolVersion` is `1`;
//! MCP's is `"2025-11-25"`. Three version schemes now coexist —
//! `PROTOCOL_VERSION` for `jan-klod`'s own clients, a date for MCP, an integer
//! here — and copying one into another is the mistake this comment exists to
//! prevent.
//!
//! **ACP is bidirectional.** The agent originates requests *to the client*:
//! `session/request_permission` blocks on the editor's answer. Everything else
//! this core serves is client→server requests plus server→client
//! notifications. That is the hard half of the port and it arrives in a later
//! box; this one is the handshake, which needs no session at all.
//!
//! # The ordering rule is the spec's, not ours
//!
//! "Before a Session can be created, Clients MUST initialize the connection by
//! calling the `initialize` method." So `session/new` before `initialize` is
//! refused rather than tolerated — the same shape as [`crate::rpc`]'s
//! negotiation guard, and for the same reason: a surface that works without its
//! handshake teaches clients to skip it.

use std::io::{BufRead, Write};

use jan_klod_protocol::jsonrpc;

/// The ACP revision this agent implements.
///
/// An **integer**. See the module docs: MCP's equivalent is a date string, and
/// the two must not be confused.
pub const ACP_VERSION: i64 = 1;

/// What this agent calls itself in `initialize`.
const AGENT_NAME: &str = "jan-klod";

/// One ACP request or notification.
///
/// Its own type rather than [`jsonrpc::Request`], which flattens into this
/// repository's own `Command` enum and would reject every ACP method name. The
/// same reason `mcp` has its own.
#[derive(Debug, serde::Deserialize)]
struct Frame {
    jsonrpc: String,
    #[serde(default)]
    id: Option<jsonrpc::Id>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// The connection's state across frames.
///
/// ACP needs state to answer correctly — whether `initialize` has happened, and
/// which session ids have been handed out — so unlike `mcp`'s stateless
/// `classify` there is a value here. It still needs no runtime, which is what
/// keeps this box's rules testable without booting one.
#[derive(Debug, Default)]
struct Connection {
    /// Whether `initialize` has been answered.
    negotiated: bool,
    /// Session ids this connection minted, most recent last.
    sessions: Vec<String>,
}

/// Serve ACP on `input`/`output` until the client hangs up.
///
/// # Errors
/// Any I/O failure on `output`. A malformed frame is answered, not returned:
/// one bad line is not a reason to hang up on an editor.
pub fn serve<R: BufRead, W: Write>(input: R, output: &mut W) -> std::io::Result<()> {
    let mut connection = Connection::default();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = connection.answer(&line) {
            write_frame(output, &response)?;
        }
    }
    Ok(())
}

impl Connection {
    /// One line to the response it earns, or `None` for a notification.
    fn answer(&mut self, line: &str) -> Option<jsonrpc::Response> {
        let frame: Frame = match serde_json::from_str(line) {
            Ok(frame) => frame,
            Err(err) => {
                return Some(refuse(
                    jsonrpc::Id::Null,
                    jsonrpc::PARSE_ERROR,
                    format!("a frame must be one JSON-RPC object on one line: {err}"),
                ));
            }
        };

        // A notification earns no answer; there is nothing to correlate one to.
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

        // The spec's MUST, enforced: nothing but `initialize` is served before
        // the handshake. A surface that works without it teaches clients to
        // skip it, and then the version negotiation is decoration.
        if !self.negotiated && frame.method != "initialize" {
            return Some(refuse(
                id,
                jsonrpc::INVALID_REQUEST,
                format!(
                    "`{}` before `initialize`: the connection is not negotiated",
                    frame.method
                ),
            ));
        }

        Some(match frame.method.as_str() {
            "initialize" => {
                self.negotiated = true;
                jsonrpc::Response::result(id, Self::initialized(&frame.params))
            }
            "session/new" => jsonrpc::Response::result(id, self.new_session(&frame.params)),
            "session/prompt" => refuse(
                id,
                jsonrpc::METHOD_NOT_FOUND,
                "`session/prompt` is not served yet; `initialize` and `session/new` are".to_owned(),
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
    /// Every capability is declared **false or empty**, which is the honest
    /// answer today: no session loading, no image or audio prompts, no
    /// embedded context, and no authentication methods. An agent that claimed
    /// them would be found out by the first editor that used one, and a
    /// capability list is exactly the thing a client is entitled to trust.
    fn initialized(params: &serde_json::Value) -> serde_json::Value {
        let asked = params
            .get("protocolVersion")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(0);
        serde_json::json!({
            "protocolVersion": ACP_VERSION,
            "agentCapabilities": {
                "loadSession": false,
                "promptCapabilities": {
                    "image": false,
                    "audio": false,
                    "embeddedContext": false,
                },
            },
            "agentInfo": {
                "name": AGENT_NAME,
                "version": env!("CARGO_PKG_VERSION"),
            },
            // No authentication: this is a subprocess the editor spawned and
            // owns, exactly as `rpc` is. There is nobody else on the pipe.
            "authMethods": [],
            "_meta": {
                "clientProtocolVersion": asked,
            },
        })
    }

    /// Mint a session id.
    ///
    /// # `cwd` is recorded and **not** honoured, deliberately
    ///
    /// ACP's `session/new` carries the editor's working directory. Letting it
    /// retarget the workspace would mean a client choosing what the file tools
    /// may reach — the workspace is a *grant*, from `config.yaml` or `$PWD`,
    /// and `Workspace::open` already refuses `$HOME` and filesystem roots
    /// because a jail that wide protects nothing. An editor saying
    /// `cwd: "/"` must not widen it.
    ///
    /// So it is kept for the mismatch to be reported rather than acted on. What
    /// to do when the editor's project is not the workspace — refuse, warn, or
    /// serve it anyway — is a decision for the box that runs a turn, since
    /// before then there is nothing it could affect.
    fn new_session(&mut self, params: &serde_json::Value) -> serde_json::Value {
        let id = crate::serve::new_session_id();
        self.sessions.push(id.clone());
        let cwd = params
            .get("cwd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        serde_json::json!({
            "sessionId": id,
            "_meta": { "clientCwd": cwd },
        })
    }
}

/// A failed answer.
fn refuse(id: jsonrpc::Id, code: i64, message: String) -> jsonrpc::Response {
    jsonrpc::Response::error(id, jsonrpc::Error::new(code, message))
}

/// Write one frame and **flush it**, for the reason [`crate::rpc`] documents:
/// stdout to a pipe is block-buffered, so without the flush the editor waits
/// for an answer sitting in this process's buffer.
fn write_frame<W: Write>(output: &mut W, response: &jsonrpc::Response) -> std::io::Result<()> {
    let text = serde_json::to_string(response).expect("a response serializes");
    output.write_all(text.as_bytes())?;
    output.write_all(b"\n")?;
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::{Connection, ACP_VERSION};
    use jan_klod_protocol::jsonrpc;

    /// A negotiated connection, since almost everything needs one.
    fn negotiated() -> Connection {
        let mut connection = Connection::default();
        let value = connection
            .answer(r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#)
            .expect("initialize is answered");
        assert!(
            serde_json::to_value(value).expect("serializes")["result"]["protocolVersion"] == 1,
            "the handshake succeeded"
        );
        connection
    }

    fn call(connection: &mut Connection, line: &str) -> serde_json::Value {
        let response = connection.answer(line).expect("a request earns an answer");
        serde_json::to_value(response).expect("it serializes")
    }

    #[test]
    fn initialize_answers_with_an_integer_version() {
        let mut connection = Connection::default();
        let value = call(
            &mut connection,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{"fs":{"readTextFile":true}}}}"#,
        );
        assert_eq!(value["result"]["protocolVersion"], ACP_VERSION);
        assert!(
            value["result"]["protocolVersion"].is_i64(),
            "an integer, not the date string MCP uses: {value}"
        );
        assert_eq!(value["result"]["agentInfo"]["name"], "jan-klod");
        assert_eq!(
            value["result"]["authMethods"],
            serde_json::json!([]),
            "no authentication: the editor spawned this process and owns it"
        );
    }

    /// Every capability is declared false, because every one of them is.
    #[test]
    fn no_capability_is_claimed_that_is_not_implemented() {
        let mut connection = Connection::default();
        let value = call(
            &mut connection,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#,
        );
        let capabilities = &value["result"]["agentCapabilities"];
        assert_eq!(capabilities["loadSession"], serde_json::Value::Bool(false));
        for claim in ["image", "audio", "embeddedContext"] {
            assert_eq!(
                capabilities["promptCapabilities"][claim],
                serde_json::Value::Bool(false),
                "{claim} is not implemented, so it is not claimed: {value}"
            );
        }
    }

    /// The spec's MUST: nothing before `initialize`.
    #[test]
    fn session_new_before_initialize_is_refused() {
        let mut connection = Connection::default();
        let value = call(
            &mut connection,
            r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}"#,
        );
        assert_eq!(value["error"]["code"], jsonrpc::INVALID_REQUEST);
        assert!(
            value["error"]["message"]
                .as_str()
                .unwrap_or_default()
                .contains("not negotiated"),
            "and it says why: {value}"
        );
    }

    #[test]
    fn session_new_returns_an_id_after_the_handshake() {
        let mut connection = negotiated();
        let value = call(
            &mut connection,
            r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp/project","mcpServers":[]}}"#,
        );
        let id = value["result"]["sessionId"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(!id.is_empty(), "an id is minted: {value}");
        assert_eq!(connection.sessions, [id], "and the connection remembers it");
    }

    /// Two sessions on one connection get different ids — otherwise a second
    /// `session/new` would silently alias the first.
    #[test]
    fn each_session_gets_its_own_id() {
        let mut connection = negotiated();
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}"#;
        let first = call(&mut connection, line)["result"]["sessionId"].clone();
        let second = call(&mut connection, line)["result"]["sessionId"].clone();
        assert_ne!(first, second, "two sessions, two ids");
        assert_eq!(connection.sessions.len(), 2);
    }

    /// The editor's `cwd` is echoed rather than acted on. The workspace is a
    /// grant; a client must not be able to retarget it.
    #[test]
    fn the_clients_cwd_is_reported_not_adopted() {
        let mut connection = negotiated();
        let value = call(
            &mut connection,
            r#"{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/","mcpServers":[]}}"#,
        );
        assert_eq!(
            value["result"]["_meta"]["clientCwd"], "/",
            "it is visible, so a mismatch can be reported: {value}"
        );
    }

    #[test]
    fn session_prompt_says_it_is_not_served_yet() {
        let mut connection = negotiated();
        let value = call(
            &mut connection,
            r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"sessionId":"x","prompt":[]}}"#,
        );
        assert_eq!(value["error"]["code"], jsonrpc::METHOD_NOT_FOUND);
    }

    #[test]
    fn a_notification_earns_no_answer() {
        let mut connection = negotiated();
        assert!(
            connection
                .answer(r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"x"}}"#)
                .is_none(),
            "a cancel is a notification"
        );
    }

    #[test]
    fn an_unreadable_frame_is_answered_with_a_null_id() {
        let mut connection = Connection::default();
        let value = call(&mut connection, "{not json");
        assert_eq!(value["error"]["code"], jsonrpc::PARSE_ERROR);
        assert!(value["id"].is_null(), "{value}");
    }
}
