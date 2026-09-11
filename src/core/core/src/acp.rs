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

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::rc::Rc;

use jan_klod_protocol::jsonrpc;

use crate::conductor::{Event, EventSink, Flow, RunResult};
use crate::AgentSession;

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
pub struct Connection {
    /// Whether `initialize` has been answered.
    negotiated: bool,
    /// Session ids this connection minted, most recent last.
    sessions: Vec<String>,
}

/// What a frame means, decided without a session.
enum Routed {
    /// A notification: nothing to answer.
    Silent,
    /// Answerable without running anything.
    Answer(jsonrpc::Response),
    /// `session/prompt`, which needs a turn.
    Prompt {
        /// The request to answer.
        id: jsonrpc::Id,
        /// Its params.
        params: serde_json::Value,
    },
}

/// Serve ACP on `input`/`output` until the client hangs up.
///
/// # Errors
/// Any I/O failure on `output`. A malformed frame is answered, not returned:
/// one bad line is not a reason to hang up on an editor.
pub fn serve<R: BufRead, W: Write>(
    input: R,
    output: W,
    agent: &mut AgentSession,
) -> std::io::Result<()> {
    // Shared because a turn streams `session/update` notifications *while* it
    // runs, so the sink and the answer both write here.
    let writer = Rc::new(RefCell::new(output));
    let mut connection = Connection::default();
    for line in input.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = connection.answer(&line, agent, &writer) {
            write_frame(&mut *writer.borrow_mut(), &response)?;
        }
    }
    Ok(())
}

/// Streams a turn's events as ACP `session/update` notifications.
struct AcpSink<'a, W: Write> {
    writer: &'a Rc<RefCell<W>>,
    session: String,
}

impl<W: Write> EventSink for AcpSink<'_, W> {
    fn emit(&mut self, event: &Event) -> Flow {
        // Only assistant text maps to an ACP update today. A tool call has its
        // own `sessionUpdate` kinds in the spec, and inventing a shape for them
        // here — rather than reading what ACP defines — is how a client ends up
        // rendering something nobody agreed on. Left for the box that needs it.
        let Event::TextDelta(text) = event else {
            return Flow::Continue;
        };
        let update = serde_json::json!({
            "sessionId": self.session,
            "update": {
                "sessionUpdate": "agent_message_chunk",
                "content": { "type": "text", "text": text },
            },
        });
        let notification = serde_json::json!({
            "jsonrpc": jsonrpc::VERSION,
            "method": "session/update",
            "params": update,
        });
        let mut writer = self.writer.borrow_mut();
        // Nobody reading means nothing left to stream, which the conductor
        // treats as a cancellation at its next loop boundary.
        if serde_json::to_writer(&mut *writer, &notification).is_err()
            || writer.write_all(b"\n").is_err()
            || writer.flush().is_err()
        {
            return Flow::Stop;
        }
        Flow::Continue
    }
}

impl Connection {
    /// A connection that has not yet been negotiated.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// One line to the response it earns, or `None` for a notification.
    ///
    /// Public so a caller can drive ACP without owning the read loop — which
    /// is what [`serve`] does, and what a test has to do: a session id is
    /// **minted by the agent**, so a client cannot script `session/prompt` in
    /// advance. It must read the id out of `session/new` first, exactly as an
    /// editor does.
    pub fn answer<W: Write>(
        &mut self,
        line: &str,
        agent: &mut AgentSession,
        writer: &Rc<RefCell<W>>,
    ) -> Option<jsonrpc::Response> {
        match self.route(line) {
            Routed::Silent => None,
            Routed::Answer(response) => Some(response),
            Routed::Prompt { id, params } => Some(match self.prompt(&params, agent, writer) {
                Ok(result) => jsonrpc::Response::result(id, result),
                Err(message) => refuse(id, jsonrpc::INTERNAL_ERROR, message),
            }),
        }
    }

    /// Everything a frame can mean **without** a session.
    ///
    /// The handshake, the ordering MUST, and every refusal are settled here, so
    /// they stay checkable with no booted runtime — the same division
    /// [`crate::mcp`]'s `classify` and [`crate::rpc`]'s `parse` have.
    fn route(&mut self, line: &str) -> Routed {
        let frame: Frame = match serde_json::from_str(line) {
            Ok(frame) => frame,
            Err(err) => {
                return Routed::Answer(refuse(
                    jsonrpc::Id::Null,
                    jsonrpc::PARSE_ERROR,
                    format!("a frame must be one JSON-RPC object on one line: {err}"),
                ));
            }
        };

        // A notification earns no answer; there is nothing to correlate one to.
        let Some(id) = frame.id else {
            return Routed::Silent;
        };

        if frame.jsonrpc != jsonrpc::VERSION {
            return Routed::Answer(refuse(
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
            return Routed::Answer(refuse(
                id,
                jsonrpc::INVALID_REQUEST,
                format!(
                    "`{}` before `initialize`: the connection is not negotiated",
                    frame.method
                ),
            ));
        }

        Routed::Answer(match frame.method.as_str() {
            "initialize" => {
                self.negotiated = true;
                jsonrpc::Response::result(id, Self::initialized(&frame.params))
            }
            "session/new" => jsonrpc::Response::result(id, self.new_session(&frame.params)),
            "session/prompt" => {
                return Routed::Prompt {
                    id,
                    params: frame.params,
                }
            }
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

impl Connection {
    /// Run one turn for `session/prompt`, streaming updates as it goes.
    ///
    /// # `stopReason` is not `isError`, and assuming otherwise would lose data
    ///
    /// ACP's stop reasons say why a turn **ended**: `end_turn`, `max_tokens`,
    /// `max_turn_requests`, `refusal`, `cancelled`. None of them means "the
    /// tool failed", which is the opposite of MCP's `isError` — so the mapping
    /// had to be read rather than carried across from [`crate::mcp`].
    ///
    /// **A permission refusal is `end_turn`, never `refusal`.** The spec is
    /// explicit that on `refusal` "the user prompt and everything that comes
    /// after it won't be included in the next prompt, so this should be
    /// reflected in the UI" — it means the agent declined the whole exchange,
    /// and an editor is entitled to discard the prompt. A blocked tool call is
    /// not that: the turn ran, the model was told, and it answered. Reporting
    /// it as `refusal` would throw away what the user typed.
    ///
    /// A turn that genuinely *failed* has no stop reason at all, so it is a
    /// JSON-RPC error — again the opposite of MCP, where a failure rides
    /// in-band on a successful result.
    ///
    /// # Errors
    /// A message when the frame is unusable or the turn failed, which the
    /// caller returns as a JSON-RPC error.
    fn prompt<W: Write>(
        &self,
        params: &serde_json::Value,
        agent: &mut AgentSession,
        writer: &Rc<RefCell<W>>,
    ) -> Result<serde_json::Value, String> {
        let Some(session) = params.get("sessionId").and_then(serde_json::Value::as_str) else {
            return Err("`session/prompt` needs a `sessionId`".to_owned());
        };
        if !self.sessions.iter().any(|known| known == session) {
            return Err(format!(
                "no session {session} on this connection; create one with `session/new`"
            ));
        }

        // The prompt is a list of content blocks. Only text is supported, and
        // `initialize` says so — `promptCapabilities` declares image, audio and
        // embedded context all false, so a client sending one was told not to.
        let text = params
            .get("prompt")
            .and_then(serde_json::Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| {
                        block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    })
                    .filter_map(|block| block.get("text").and_then(serde_json::Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        if text.trim().is_empty() {
            return Err("`prompt` carried no text content".to_owned());
        }

        let mut sink = AcpSink {
            writer,
            session: session.to_owned(),
        };
        // `run_streaming_headless` for now: an editor *can* answer a
        // permission request, but asking it needs the agent→client direction,
        // which is the next box. Until then each prompt takes its default,
        // which is a refusal — the same posture as MCP.
        match agent.run_streaming_headless(&mut sink, session, &text) {
            RunResult::Answered { .. } => Ok(serde_json::json!({ "stopReason": "end_turn" })),
            RunResult::Failed(message) => Err(message),
        }
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
    use super::{Connection, Routed, ACP_VERSION};
    use jan_klod_protocol::jsonrpc;

    /// Everything except `session/prompt` is answered without touching the
    /// agent, so these tests pass no runtime at all — the reason `answer` takes
    /// the session by argument rather than holding one.
    fn negotiated() -> Connection {
        let mut connection = Connection::default();
        let value = call(
            &mut connection,
            r#"{"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#,
        );
        assert_eq!(
            value["result"]["protocolVersion"], 1,
            "the handshake succeeded"
        );
        connection
    }

    /// Answer a line that does not need a turn.
    ///
    /// `session/prompt` is the only method that touches the agent, and it is
    /// checked in `host/tests/it/acp.rs` where a real one exists — so these
    /// call the frame handling directly and never reach it.
    fn call(connection: &mut Connection, line: &str) -> serde_json::Value {
        match connection.route(line) {
            Routed::Answer(response) => serde_json::to_value(response).expect("it serializes"),
            Routed::Silent => panic!("{line} earns an answer"),
            Routed::Prompt { .. } => panic!("{line} needs a turn; check it through the harness"),
        }
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
    fn session_prompt_is_routed_to_a_turn() {
        let mut connection = negotiated();
        assert!(
            matches!(
                connection.route(
                    r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"sessionId":"x","prompt":[]}}"#
                ),
                Routed::Prompt { .. }
            ),
            "a prompt needs a session, so it is routed rather than answered"
        );
    }

    /// But it is still refused before the handshake — the ordering MUST applies
    /// to the method that matters most, not only to `session/new`.
    #[test]
    fn session_prompt_before_initialize_is_refused() {
        let mut connection = Connection::default();
        let value = call(
            &mut connection,
            r#"{"jsonrpc":"2.0","id":1,"method":"session/prompt","params":{"sessionId":"x","prompt":[]}}"#,
        );
        assert_eq!(value["error"]["code"], jsonrpc::INVALID_REQUEST);
    }

    #[test]
    fn a_notification_earns_no_answer() {
        let mut connection = negotiated();
        assert!(
            matches!(
                connection.route(
                    r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"x"}}"#
                ),
                Routed::Silent
            ),
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
