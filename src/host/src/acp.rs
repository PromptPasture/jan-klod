//! ACP: Agent Client Protocol for editor↔agent (Zed's standard).
//! Version is integer (not date); bidirectional (agent requests client).
//! Sessions require `initialize` first.

use std::cell::RefCell;
use std::io::{BufRead, Write};
use std::rc::Rc;

use jan_klod_protocol::jsonrpc;

use std::sync::mpsc::Receiver;

use jan_klod_core::conductor::{Event, EventSink, Flow, RunResult};
use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::AgentSession;

/// ACP protocol version (integer, not date like MCP).
pub const ACP_VERSION: i64 = 1;

/// What this agent calls itself in `initialize`.
const AGENT_NAME: &str = "jan-klod";

/// One ACP request or notification (own type to avoid name conflicts with
/// repository's `Command` enum, like `mcp` does).
#[derive(Debug, serde::Deserialize)]
struct Frame {
    jsonrpc: String,
    #[serde(default)]
    id: Option<jsonrpc::Id>,
    method: String,
    #[serde(default)]
    params: serde_json::Value,
}

/// Connection state (handshake, session ids). Unlike `mcp`'s stateless
/// `classify`, this tracks initialization and minted sessions. No runtime
/// needed; rules testable without booting one.
#[derive(Debug, Default)]
pub struct Connection {
    /// Whether `initialize` has been answered.
    negotiated: bool,
    /// Session ids this connection minted, most recent last.
    sessions: Vec<String>,
}

/// Agent→editor question interface (seam for testability).
///
/// Hard part of `session/request_permission` is bidirectional blocking: agent
/// writes and reads on the same pipe. [`serve`] needs it; tests need scripted
/// answers. Splitting them keeps protocol shape testable without threads.
pub trait Asker {
    /// Put `prompt` to the editor and return the answer.
    /// Default answer is always safe (headless refusal).
    ///
    /// Uses `&self` with interior mutability: streaming sink and permission
    /// driver both need this during one turn (can't lend mutably twice).
    fn ask(&self, session: &str, prompt: &UserPrompt) -> String;

    /// Whether the client cancelled (sticky once true).
    fn cancelled(&self) -> bool {
        false
    }
}

/// [`Asker`] that never asks (headless refusal, like [`jan_klod_core::HeadlessDriver`]).
pub struct NoAsker;
impl Asker for NoAsker {
    fn ask(&self, _session: &str, prompt: &UserPrompt) -> String {
        prompt.default_answer.clone()
    }
}

/// Turns the loop's `ask` into ACP's `session/request_permission`.
struct AcpDriver<'a, A: Asker> {
    asker: &'a A,
    session: String,
}

impl<A: Asker> Driver for AcpDriver<'_, A> {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        self.asker.ask(&self.session, prompt)
    }
}

/// Whether a line is a `session/cancel` notification.
fn is_cancel(line: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(line).is_ok_and(|frame| {
        frame.get("method").and_then(serde_json::Value::as_str) == Some("session/cancel")
    })
}

/// The `options` array for a permission request, built from the prompt's own.
///
/// The `optionId` **is** the interceptor's option text, so nothing translates
/// between two vocabularies: whatever the editor picks is already one of the
/// answers the gate offered. A translation table here would be a second place
/// for the permission model to drift from itself.
///
/// `kind` is ACP's hint for how to render a choice, so an affirmative is
/// `allow_once` and everything else `reject_once`. Getting it wrong is
/// cosmetic; getting `optionId` wrong would answer a different question.
fn permission_options(prompt: &UserPrompt) -> serde_json::Value {
    let offered: Vec<&str> = if prompt.options.is_empty() {
        // A free-text prompt still has to be answerable, and an editor cannot
        // type into a permission dialog — so it is offered the default and a
        // refusal, which is the honest pair.
        vec![prompt.default_answer.as_str()]
    } else {
        prompt.options.iter().map(String::as_str).collect()
    };
    serde_json::Value::Array(
        offered
            .into_iter()
            .map(|option| {
                let affirmative = matches!(option, "yes" | "always" | "allow");
                serde_json::json!({
                    "optionId": option,
                    "name": option,
                    "kind": if affirmative { "allow_once" } else { "reject_once" },
                })
            })
            .collect(),
    )
}

/// [`Asker`] for [`serve`]: write request, block on answer (agent→client).
/// Only place core originates JSON-RPC and waits. Works because reader thread
/// holds the pipe while the turn runs.
struct PipeAsker<'a, W: Write> {
    writer: Rc<RefCell<W>>,
    incoming: &'a Receiver<String>,
    next_id: std::cell::Cell<i64>,
    /// Set the moment a `session/cancel` is seen, and never unset.
    cancelled: std::cell::Cell<bool>,
}

impl<W: Write> PipeAsker<'_, W> {
    /// Drain editor input, noting cancels (checked between streamed events).
    fn drain(&self) {
        while let Ok(line) = self.incoming.try_recv() {
            if is_cancel(&line) {
                self.cancelled.set(true);
            }
        }
    }
}

impl<W: Write> Asker for PipeAsker<'_, W> {
    fn cancelled(&self) -> bool {
        self.drain();
        self.cancelled.get()
    }

    fn ask(&self, session: &str, prompt: &UserPrompt) -> String {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        let request = serde_json::json!({
            "jsonrpc": jsonrpc::VERSION,
            "id": id,
            "method": "session/request_permission",
            "params": {
                "sessionId": session,
                "toolCall": { "toolCallId": format!("perm-{id}") },
                "options": permission_options(prompt),
                "_meta": { "question": prompt.question },
            },
        });
        {
            let mut writer = self.writer.borrow_mut();
            if serde_json::to_writer(&mut *writer, &request).is_err()
                || writer.write_all(b"\n").is_err()
                || writer.flush().is_err()
            {
                // Broken pipe: default is safe refusal.
                return prompt.default_answer.clone();
            }
        }

        // Read until answer arrives; cancel ends wait and sets flag for stopReason.
        while let Ok(line) = self.incoming.recv() {
            let Ok(frame) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if is_cancel(&line) {
                self.cancelled.set(true);
                return prompt.default_answer.clone();
            }
            if frame.get("id").and_then(serde_json::Value::as_i64) != Some(id) {
                continue;
            }
            let outcome = &frame["result"]["outcome"];
            return match outcome.get("outcome").and_then(serde_json::Value::as_str) {
                Some("selected") => outcome
                    .get("optionId")
                    .and_then(serde_json::Value::as_str)
                    .map_or_else(|| prompt.default_answer.clone(), str::to_owned),
                // Cancelled or unreadable: default (never widen permissions).
                _ => prompt.default_answer.clone(),
            };
        }
        // EOF: editor gone.
        prompt.default_answer.clone()
    }
}

/// Frame routing (session-independent classification).
enum Routed {
    /// Notification (no answer).
    Silent,
    /// Answer without running (handshake, routing).
    Answer(jsonrpc::Response),
    /// `session/prompt` (needs a turn).
    Prompt {
        /// Request id.
        id: jsonrpc::Id,
        /// Request params.
        params: serde_json::Value,
    },
}

/// Serve ACP on `input`/`output` until client hangs up.
///
/// # Errors
/// I/O failure on `output`. Malformed frames answered, not returned (one bad
/// line doesn't hang up an editor).
pub fn serve<R: BufRead + Send + 'static, W: Write>(
    input: R,
    output: W,
    agent: &mut AgentSession,
) -> std::io::Result<()> {
    // Writer shared: turn streams `session/update` while running.
    let writer = Rc::new(RefCell::new(output));

    // Reader thread: hands lines over. Holds no session (`AgentSession` is `!Send`).
    // Division makes editor's answer readable: while turn runs, this thread
    // holds the pipe (same pattern as `rpc`).
    let (handover, incoming) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in input.lines() {
            let handed = match line {
                Ok(line) => handover.send(line),
                // UTF-8 error: skip frame, continue.
                Err(err) if err.kind() == std::io::ErrorKind::InvalidData => continue,
                Err(_) => break,
            };
            if handed.is_err() {
                break;
            }
        }
    });

    let mut connection = Connection::default();
    let asker = PipeAsker {
        writer: Rc::clone(&writer),
        incoming: &incoming,
        // Agent-chosen IDs, well clear of editor IDs (separate id spaces).
        next_id: std::cell::Cell::new(1_000_000),
        cancelled: std::cell::Cell::new(false),
    };
    // recv ends when reader thread drops its end (EOF).
    while let Ok(line) = incoming.recv() {
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = connection.answer(&line, agent, &writer, &asker) {
            write_frame(&mut *writer.borrow_mut(), &response)?;
        }
    }
    Ok(())
}

/// Streams a turn's events as ACP `session/update` notifications.
struct AcpSink<'a, W: Write, A: Asker> {
    writer: &'a Rc<RefCell<W>>,
    session: String,
    asker: &'a A,
}

impl<W: Write, A: Asker> EventSink for AcpSink<'_, W, A> {
    fn emit(&mut self, event: &Event) -> Flow {
        // Only assistant text → ACP update today. Tool calls have `sessionUpdate`
        // kinds in the spec; inventing shapes here breaks clients.
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
        {
            let mut writer = self.writer.borrow_mut();
            // Write failure = no reader = cancellation at conductor boundary.
            if serde_json::to_writer(&mut *writer, &notification).is_err()
                || writer.write_all(b"\n").is_err()
                || writer.flush().is_err()
            {
                return Flow::Stop;
            }
        }
        // Cancel read between events (only place it's noticed, conductor boundary).
        if self.asker.cancelled() {
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

    /// Line to response or `None` for notifications.
    /// Public so callers can drive ACP without owning the read loop.
    /// Tests must read session ids from `session/new` (agent-minted).
    pub fn answer<W: Write, A: Asker>(
        &mut self,
        line: &str,
        agent: &mut AgentSession,
        writer: &Rc<RefCell<W>>,
        asker: &A,
    ) -> Option<jsonrpc::Response> {
        match self.route(line) {
            Routed::Silent => None,
            Routed::Answer(response) => Some(response),
            Routed::Prompt { id, params } => {
                Some(match self.prompt(&params, agent, writer, asker) {
                    Ok(result) => jsonrpc::Response::result(id, result),
                    Err(message) => refuse(id, jsonrpc::INTERNAL_ERROR, message),
                })
            }
        }
    }

    /// Frame routing (session-independent). Handshake, ordering MUST, refusals
    /// (testable without runtime, like `mcp::classify` and `rpc::parse`).
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

        // Notification: no answer.
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

        // Handshake MUST: only `initialize` before negotiation.
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

    /// `initialize` result (all capabilities false/empty: honest today, and
    /// capability lists are what clients trust).
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
            // No auth: subprocess the editor owns (like `rpc`); nobody else on pipe.
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
        let id = jan_klod_core::session::new_session_id();
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
    fn prompt<W: Write, A: Asker>(
        &self,
        params: &serde_json::Value,
        agent: &mut AgentSession,
        writer: &Rc<RefCell<W>>,
        asker: &A,
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
            asker,
        };
        // The editor answers a permission request through `asker`, so a
        // granted write actually happens — unlike MCP, where nobody can answer.
        let mut driver = AcpDriver {
            asker,
            session: session.to_owned(),
        };
        let outcome = agent.run_streaming_with_driver(&mut driver, &mut sink, session, &text);
        // Cancellation wins over how the turn happened to finish. The spec is
        // emphatic: `cancelled` "MUST be returned when the client sends a
        // `session/cancel` notification, **even if the cancellation causes
        // exceptions in underlying operations**" — so a cancelled turn is
        // answered, not left hanging and not reported as a failure.
        if asker.cancelled() {
            return Ok(serde_json::json!({ "stopReason": "cancelled" }));
        }
        match outcome {
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
    use super::{Asker, Connection, PipeAsker, Rc, RefCell, Routed, UserPrompt, ACP_VERSION};
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

    // ---- The agent→client direction, on the wire ----

    /// Ask through a real [`PipeAsker`] with the answer already queued.
    ///
    /// The integration tests use a scripted `Asker`, which checks that the loop
    /// honours an answer. These check the **wire shape** — what is written, and
    /// how an ACP outcome maps back — which a scripted asker bypasses entirely.
    fn asked(answer: &str, prompt: &UserPrompt) -> (serde_json::Value, String) {
        let (handover, incoming) = std::sync::mpsc::channel();
        handover.send(answer.to_owned()).expect("queues the answer");
        let writer = Rc::new(RefCell::new(Vec::new()));
        let asker = PipeAsker {
            writer: Rc::clone(&writer),
            incoming: &incoming,
            next_id: std::cell::Cell::new(7),
            cancelled: std::cell::Cell::new(false),
        };
        let given = asker.ask("sess-1", prompt);
        let written = String::from_utf8(writer.borrow().clone()).expect("UTF-8");
        let frame = serde_json::from_str(written.trim()).expect("one frame was written");
        (frame, given)
    }

    fn confirm() -> UserPrompt {
        UserPrompt {
            question: "write config.yaml?".to_owned(),
            options: vec!["yes".to_owned(), "no".to_owned(), "always".to_owned()],
            default_answer: "no".to_owned(),
        }
    }

    #[test]
    fn a_permission_request_carries_the_gates_own_options() {
        let (frame, given) = asked(
            r#"{"jsonrpc":"2.0","id":7,"result":{"outcome":{"outcome":"selected","optionId":"always"}}}"#,
            &confirm(),
        );
        assert_eq!(frame["method"], "session/request_permission");
        assert_eq!(frame["id"], 7);
        assert_eq!(frame["params"]["sessionId"], "sess-1");

        // The `optionId`s are the gate's own answers, so nothing translates
        // between two vocabularies.
        let ids: Vec<&str> = frame["params"]["options"]
            .as_array()
            .expect("options")
            .iter()
            .map(|o| o["optionId"].as_str().unwrap_or_default())
            .collect();
        assert_eq!(ids, ["yes", "no", "always"]);
        assert_eq!(frame["params"]["options"][0]["kind"], "allow_once");
        assert_eq!(frame["params"]["options"][1]["kind"], "reject_once");

        assert_eq!(
            given, "always",
            "the selected optionId is handed back to the gate verbatim"
        );
    }

    /// Every answer this cannot read takes the default, which refuses.
    ///
    /// The direction matters: an unreadable answer must never *widen* a
    /// permission. A client that sends nonsense, or an outcome shape from a
    /// future revision, gets a refusal rather than a grant.
    #[test]
    fn an_answer_that_cannot_be_read_refuses() {
        for answer in [
            r#"{"jsonrpc":"2.0","id":7,"result":{"outcome":{"outcome":"cancelled"}}}"#,
            r#"{"jsonrpc":"2.0","id":7,"result":{"outcome":{"outcome":"selected"}}}"#,
            r#"{"jsonrpc":"2.0","id":7,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":7,"error":{"code":-1,"message":"no"}}"#,
        ] {
            let (_, given) = asked(answer, &confirm());
            assert_eq!(given, "no", "{answer} must not grant anything");
        }
    }

    /// A `session/cancel` arriving while parked ends the wait with a refusal.
    ///
    /// The spec requires a cancelled outcome for every pending permission
    /// request. Answering the parked `session/prompt` with
    /// `stopReason: cancelled` is a later box; not hanging is this one's.
    /// A cancel while parked refuses the permission **and** marks the turn
    /// cancelled, which is what turns the parked prompt's answer from
    /// `end_turn` into `cancelled`.
    #[test]
    fn a_cancel_while_parked_refuses_and_marks_the_turn() {
        let (handover, incoming) = std::sync::mpsc::channel();
        handover
            .send(
                r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"sess-1"}}"#
                    .to_owned(),
            )
            .expect("queues the cancel");
        let asker = PipeAsker {
            writer: Rc::new(RefCell::new(Vec::new())),
            incoming: &incoming,
            next_id: std::cell::Cell::new(7),
            cancelled: std::cell::Cell::new(false),
        };
        // Not asserting `!cancelled()` first: `cancelled()` drains, and the
        // cancel is already queued — so it is legitimately true straight away.
        // What matters is that `ask` refuses *and* the turn is marked.
        assert_eq!(
            asker.ask("sess-1", &confirm()),
            "no",
            "the permission is refused"
        );
        assert!(asker.cancelled(), "and the turn is marked cancelled");
    }

    /// A cancel arriving while the turn *streams* is noticed too — between
    /// events is the only other place it can be.
    #[test]
    fn a_cancel_between_events_is_noticed_without_waiting() {
        let (handover, incoming) = std::sync::mpsc::channel();
        let asker = PipeAsker {
            writer: Rc::new(RefCell::new(Vec::new())),
            incoming: &incoming,
            next_id: std::cell::Cell::new(1),
            cancelled: std::cell::Cell::new(false),
        };
        assert!(!asker.cancelled());
        handover
            .send(
                r#"{"jsonrpc":"2.0","method":"session/cancel","params":{"sessionId":"s"}}"#
                    .to_owned(),
            )
            .expect("queues it");
        assert!(asker.cancelled(), "drained without blocking");
        // Sticky: a cancel cannot be un-seen by a later empty drain.
        assert!(asker.cancelled());
    }

    /// An ordinary frame arriving mid-turn is not a cancel, and must not be
    /// mistaken for one — a client is allowed to talk while a turn runs.
    #[test]
    fn an_unrelated_frame_mid_turn_is_not_a_cancel() {
        let (handover, incoming) = std::sync::mpsc::channel();
        let asker = PipeAsker {
            writer: Rc::new(RefCell::new(Vec::new())),
            incoming: &incoming,
            next_id: std::cell::Cell::new(1),
            cancelled: std::cell::Cell::new(false),
        };
        handover
            .send(r#"{"jsonrpc":"2.0","id":9,"method":"session/new","params":{}}"#.to_owned())
            .expect("queues it");
        handover
            .send("not even json".to_owned())
            .expect("queues it");
        assert!(!asker.cancelled(), "neither frame cancels anything");
    }

    /// An editor that closes the pipe while parked does not hang the turn.
    #[test]
    fn a_closed_pipe_while_parked_refuses_rather_than_hanging() {
        let (handover, incoming) = std::sync::mpsc::channel::<String>();
        drop(handover); // the editor is gone
        let writer = Rc::new(RefCell::new(Vec::new()));
        let asker = PipeAsker {
            writer,
            incoming: &incoming,
            next_id: std::cell::Cell::new(1),
            cancelled: std::cell::Cell::new(false),
        };
        assert_eq!(asker.ask("sess-1", &confirm()), "no");
    }

    /// A free-text prompt still has to be answerable by a dialog.
    #[test]
    fn a_free_text_prompt_offers_its_default_as_an_option() {
        let prompt = UserPrompt {
            question: "which branch?".to_owned(),
            options: vec![],
            default_answer: "main".to_owned(),
        };
        let (frame, _) = asked(
            r#"{"jsonrpc":"2.0","id":7,"result":{"outcome":{"outcome":"selected","optionId":"main"}}}"#,
            &prompt,
        );
        assert_eq!(frame["params"]["options"][0]["optionId"], "main");
    }

    #[test]
    fn an_unreadable_frame_is_answered_with_a_null_id() {
        let mut connection = Connection::default();
        let value = call(&mut connection, "{not json");
        assert_eq!(value["error"]["code"], jsonrpc::PARSE_ERROR);
        assert!(value["id"].is_null(), "{value}");
    }
}
