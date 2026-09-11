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
    ///
    /// `NoAsker` — this editor never answers a permission request, so every
    /// prompt takes its default. The tests that *do* answer supply their own.
    fn send(
        &mut self,
        agent: &mut jan_klod_core::AgentSession,
        line: &str,
    ) -> Option<serde_json::Value> {
        self.send_with(agent, line, &mut jan_klod_core::acp::NoAsker)
    }

    /// The same, with a chosen [`jan_klod_core::acp::Asker`].
    fn send_with<A: jan_klod_core::acp::Asker>(
        &mut self,
        agent: &mut jan_klod_core::AgentSession,
        line: &str,
        asker: &mut A,
    ) -> Option<serde_json::Value> {
        self.connection
            .answer(line, agent, &self.writer, asker)
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

// ---- The agent→client direction (#57 box 3) ----

/// An editor that answers every permission request with `answer`, recording
/// what it was asked.
struct Answering {
    answer: String,
    asked: Vec<String>,
}

impl jan_klod_core::acp::Asker for Answering {
    fn ask(&mut self, _session: &str, prompt: &jan_klod_core::intercept::UserPrompt) -> String {
        self.asked.push(prompt.question.clone());
        // A real editor picks an `optionId`, and those are the prompt's own
        // options — so answering with one is answering as ACP would.
        assert!(
            prompt.options.is_empty() || prompt.options.contains(&self.answer),
            "the answer must be one of the options offered: {:?}",
            prompt.options
        );
        self.answer.clone()
    }
}

/// Boot an agent whose provider asks for a real workspace write, then answers.
fn booted_writing(
    tag: &str,
) -> Option<(
    common::TempDir,
    jan_klod_core::AgentSession,
    std::path::PathBuf,
)> {
    const GUESTS: [&str; 4] = [
        "provider-openai.wasm",
        "interceptor-permission.wasm",
        "interceptor-tool-selector.wasm",
        "tool-fs.wasm",
    ];
    if !common::guests_staged(&GUESTS) {
        return None;
    }
    let dir =
        common::TempDir(std::env::temp_dir().join(format!("jk-acp-{tag}-{}", std::process::id())));
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let target = dir.0.join("written-by-the-editor.txt");
    let config = dir.0.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
workspace: {}
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    tool-selector:
      enabled: true
    permission:
      enabled: true
  tool:
    fs:
      enabled: true
",
            dir.0.display()
        ),
    )
    .expect("writes the config");

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let http = move || -> jan_klod_core::route::HttpFn {
        let calls = std::sync::Arc::clone(&calls);
        Box::new(move |_m, _u, _h, _b, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[{
                    "id":"w1",
                    "function":{
                        "name":"fs",
                        "arguments":"{\"op\":\"write\",\"path\":\"written-by-the-editor.txt\",\"contents\":\"granted\"}"
                    }}]},"finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"done"},"finish_reason":"stop"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).expect("serializes"),
            })
        })
    };
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let agent = runtime.build_agent(&http).expect("agent boots");
    Some((dir, agent, target))
}

/// An editor that grants the permission gets the write.
///
/// The assertion is the **file**, not that the turn continued — a turn
/// continues either way, and "the editor was asked" proves only that a question
/// was posed. Only the file says the answer was honoured.
#[test]
fn an_editor_that_grants_permission_gets_the_write() {
    let Some((_dir, mut agent, target)) = booted_writing("grant") else {
        return;
    };
    let mut editor = Editor::new();
    let session = editor.opened(&mut agent);
    let mut answering = Answering {
        answer: "yes".to_owned(),
        asked: Vec::new(),
    };

    let answer = editor
        .send_with(
            &mut agent,
            &format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session}","prompt":[{{"type":"text","text":"write it"}}]}}}}"#
            ),
            &mut answering,
        )
        .expect("the prompt is answered");

    assert!(
        !answering.asked.is_empty(),
        "the editor was asked at all: {answer}"
    );
    assert!(
        target.exists(),
        "and the answer was honoured — {} exists",
        target.display()
    );
    assert_eq!(
        std::fs::read_to_string(&target).expect("reads it").trim(),
        "granted",
        "with the contents the model asked for"
    );
    assert_eq!(
        answer["result"]["stopReason"], "end_turn",
        "a granted turn ends normally: {answer}"
    );
}

/// The same turn, refused: the editor is asked, says no, and nothing is written.
///
/// The pair is the point. One test showing the file present and another showing
/// it absent, from the same provider script, is what proves the *answer* decides
/// — rather than the tool never having been reached.
#[test]
fn an_editor_that_refuses_permission_prevents_the_write() {
    let Some((_dir, mut agent, target)) = booted_writing("refuse") else {
        return;
    };
    let mut editor = Editor::new();
    let session = editor.opened(&mut agent);
    let mut answering = Answering {
        answer: "no".to_owned(),
        asked: Vec::new(),
    };

    let answer = editor
        .send_with(
            &mut agent,
            &format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session}","prompt":[{{"type":"text","text":"write it"}}]}}}}"#
            ),
            &mut answering,
        )
        .expect("the prompt is answered");

    assert!(!answering.asked.is_empty(), "the editor was asked");
    assert!(
        !target.exists(),
        "and the refusal held: {} does not exist",
        target.display()
    );
    assert_eq!(
        answer["result"]["stopReason"], "end_turn",
        "a refused *tool* is not a refused *turn* — `refusal` would tell the \
         editor to discard the user's prompt: {answer}"
    );
}
