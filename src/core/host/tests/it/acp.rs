//! The ACP port, driven as an editor drives it.
//!
//! `core::acp`'s own tests cover the frame rules — the handshake, the ordering
//! MUST, every refusal — without a runtime. What only this module can show is a
//! prompt that runs a **real turn**, streams `session/update` notifications,
//! and then answers with a stop reason.
//!
//! The frames are fed one at a time rather than as a script, because that is
//! what a client must do: the **agent** mints the session id, so `session/new`
//! has to be read before `session/prompt` can name it.

use std::cell::RefCell;
use std::rc::Rc;

use jan_klod_core::acp::Connection;
use jan_klod_core::Runtime;

use crate::common;

fn booted(
    tag: &str,
    reply: &'static str,
) -> Option<(common::TempDir, jan_klod_core::AgentSession)> {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return None;
    }
    let dir =
        common::TempDir(std::env::temp_dir().join(format!("jk-acp-{tag}-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let config = dir.0.join("config.yaml");
    std::fs::write(
        &config,
        "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    intent-router:
      enabled: true
",
    )
    .expect("writes the config");
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let http = || common::canned_http(reply);
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((dir, agent))
}

/// An editor: one connection, frames fed in order, everything written captured.
struct Editor {
    connection: Connection,
    writer: Rc<RefCell<Vec<u8>>>,
}

impl Editor {
    fn new() -> Self {
        Self {
            connection: Connection::new(),
            writer: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// Send one frame and return the response it earned, if any.
    fn send(
        &mut self,
        agent: &mut jan_klod_core::AgentSession,
        line: &str,
    ) -> Option<serde_json::Value> {
        self.connection
            .answer(line, agent, &self.writer)
            .map(|response| serde_json::to_value(response).expect("it serializes"))
    }

    /// Every notification the agent wrote to the pipe, in order.
    fn notifications(&self) -> Vec<serde_json::Value> {
        let bytes = self.writer.borrow().clone();
        String::from_utf8(bytes)
            .expect("frames are UTF-8")
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|err| panic!("{line:?} is not a frame: {err}"))
            })
            .collect()
    }

    /// Handshake, then a session, returning its id.
    fn opened(&mut self, agent: &mut jan_klod_core::AgentSession) -> String {
        let hello = self
            .send(
                agent,
                r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}"#,
            )
            .expect("initialize is answered");
        assert_eq!(hello["result"]["protocolVersion"], 1, "{hello}");
        let new = self
            .send(
                agent,
                r#"{"jsonrpc":"2.0","id":2,"method":"session/new","params":{"cwd":"/tmp","mcpServers":[]}}"#,
            )
            .expect("session/new is answered");
        new["result"]["sessionId"]
            .as_str()
            .expect("a session id")
            .to_owned()
    }
}

/// A prompt runs a turn, streams an update, and answers `end_turn`.
#[test]
fn a_prompt_streams_an_update_and_ends_the_turn() {
    let Some((_dir, mut agent)) = booted("prompt", "the model's reply") else {
        return;
    };
    let mut editor = Editor::new();
    let session = editor.opened(&mut agent);

    let answer = editor
        .send(
            &mut agent,
            &format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session}","prompt":[{{"type":"text","text":"hello"}}]}}}}"#
            ),
        )
        .expect("session/prompt is answered");

    assert_eq!(
        answer["result"]["stopReason"], "end_turn",
        "the turn ended normally: {answer}"
    );
    assert!(answer.get("error").is_none(), "{answer}");

    // The streamed half. Only `session/update` reaches the pipe during a turn,
    // and it must carry the session it belongs to — an editor with two sessions
    // open has no other way to route it.
    let updates = editor.notifications();
    assert!(!updates.is_empty(), "the turn streamed something");
    let chunk = updates
        .iter()
        .find(|frame| frame["method"] == "session/update")
        .expect("an update was sent");
    assert_eq!(chunk["params"]["sessionId"], session);
    assert_eq!(
        chunk["params"]["update"]["sessionUpdate"],
        "agent_message_chunk"
    );
    assert_eq!(
        chunk["params"]["update"]["content"]["text"], "the model's reply",
        "the model's text is what streams: {chunk}"
    );
    assert!(
        chunk.get("id").is_none(),
        "an update is a notification, so it carries no id: {chunk}"
    );
}

/// A prompt naming a session this connection never created is refused.
///
/// Worth its own test because the alternative — running a turn on an id the
/// agent never minted — would silently create one, and then an editor's typo
/// becomes a new conversation rather than an error.
#[test]
fn a_prompt_for_an_unknown_session_is_refused() {
    let Some((_dir, mut agent)) = booted("unknown", "unused") else {
        return;
    };
    let mut editor = Editor::new();
    let _ = editor.opened(&mut agent);

    let answer = editor
        .send(
            &mut agent,
            r#"{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{"sessionId":"never-minted","prompt":[{"type":"text","text":"hi"}]}}"#,
        )
        .expect("it is answered");
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no session"),
        "refused, naming the problem: {answer}"
    );
}

/// A prompt carrying no text is refused rather than running an empty turn.
///
/// `initialize` declares `image`, `audio` and `embeddedContext` all false, so a
/// client sending only those was told not to — and an empty turn would burn a
/// provider call to answer nothing.
#[test]
fn a_prompt_with_no_text_content_is_refused() {
    let Some((_dir, mut agent)) = booted("empty", "unused") else {
        return;
    };
    let mut editor = Editor::new();
    let session = editor.opened(&mut agent);

    let answer = editor
        .send(
            &mut agent,
            &format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session}","prompt":[{{"type":"image"}}]}}}}"#
            ),
        )
        .expect("it is answered");
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no text"),
        "refused for want of text: {answer}"
    );
}
