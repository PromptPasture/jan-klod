//! The ACP port driven as an editor drives it, with frames fed one at a time because the agent mints the session id.

use std::cell::RefCell;
use std::rc::Rc;

use jk_agent::Runtime;
use jk_host::acp::Connection;

use crate::common;

fn booted(tag: &str, reply: &'static str) -> Option<(common::TempDir, jk_agent::AgentSession)> {
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

    /// Sends one frame. `NoAsker` never answers permission requests, so prompts take their default.
    fn send(
        &mut self,
        agent: &mut jk_agent::AgentSession,
        line: &str,
    ) -> Option<serde_json::Value> {
        self.send_with(agent, line, &jk_host::acp::NoAsker)
    }

    fn send_with<A: jk_host::acp::Asker>(
        &mut self,
        agent: &mut jk_agent::AgentSession,
        line: &str,
        asker: &A,
    ) -> Option<serde_json::Value> {
        self.connection
            .answer(line, agent, &self.writer, asker)
            .map(|response| serde_json::to_value(response).expect("it serializes"))
    }

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

    fn opened(&mut self, agent: &mut jk_agent::AgentSession) -> String {
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

struct Answering {
    answer: String,
    /// `RefCell` because `Asker::ask` takes `&self`.
    asked: RefCell<Vec<String>>,
    cancel: bool,
}

impl Answering {
    fn replying(answer: &str) -> Self {
        Self {
            answer: answer.to_owned(),
            asked: RefCell::new(Vec::new()),
            cancel: false,
        }
    }
}

impl jk_host::acp::Asker for Answering {
    fn ask(&self, _session: &str, prompt: &jk_protocol::turn::UserPrompt) -> String {
        self.asked.borrow_mut().push(prompt.question.clone());
        assert!(
            prompt.options.is_empty() || prompt.options.contains(&self.answer),
            "the answer must be one of the options offered: {:?}",
            prompt.options
        );
        self.answer.clone()
    }

    fn cancelled(&self) -> bool {
        self.cancel
    }
}

fn booted_writing(
    tag: &str,
) -> Option<(common::TempDir, jk_agent::AgentSession, std::path::PathBuf)> {
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
    let http = move || -> jk_wasm::route::HttpFn {
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
            Ok(jk_wasm::http::WireResponse {
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

#[test]
fn an_editor_that_grants_permission_gets_the_write() {
    let Some((_dir, mut agent, target)) = booted_writing("grant") else {
        return;
    };
    let mut editor = Editor::new();
    let session = editor.opened(&mut agent);
    let answering = Answering::replying("yes");

    let answer = editor
        .send_with(
            &mut agent,
            &format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session}","prompt":[{{"type":"text","text":"write it"}}]}}}}"#
            ),
            &answering,
        )
        .expect("the prompt is answered");

    assert!(
        !answering.asked.borrow().is_empty(),
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

#[test]
fn an_editor_that_refuses_permission_prevents_the_write() {
    let Some((_dir, mut agent, target)) = booted_writing("refuse") else {
        return;
    };
    let mut editor = Editor::new();
    let session = editor.opened(&mut agent);
    let answering = Answering::replying("no");

    let answer = editor
        .send_with(
            &mut agent,
            &format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session}","prompt":[{{"type":"text","text":"write it"}}]}}}}"#
            ),
            &answering,
        )
        .expect("the prompt is answered");

    assert!(!answering.asked.borrow().is_empty(), "the editor was asked");
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

#[test]
fn a_cancelled_turn_is_answered_cancelled() {
    let Some((_dir, mut agent, target)) = booted_writing("cancel") else {
        return;
    };
    let mut editor = Editor::new();
    let session = editor.opened(&mut agent);

    let cancelling = Answering {
        answer: "no".to_owned(),
        asked: RefCell::new(Vec::new()),
        cancel: true,
    };

    let answer = editor
        .send_with(
            &mut agent,
            &format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session}","prompt":[{{"type":"text","text":"write it"}}]}}}}"#
            ),
            &cancelling,
        )
        .expect("the parked prompt is answered rather than abandoned");

    assert_eq!(
        answer["result"]["stopReason"], "cancelled",
        "cancellation overrides the turn's outcome: {answer}"
    );
    assert!(
        answer.get("error").is_none(),
        "and it is an answer, not an error: {answer}"
    );
    assert!(
        !target.exists(),
        "nothing was written: {}",
        target.display()
    );
}

#[test]
fn a_refusal_without_a_cancel_still_ends_the_turn() {
    let Some((_dir, mut agent, _target)) = booted_writing("nocancel") else {
        return;
    };
    let mut editor = Editor::new();
    let session = editor.opened(&mut agent);
    let answering = Answering::replying("no");

    let answer = editor
        .send_with(
            &mut agent,
            &format!(
                r#"{{"jsonrpc":"2.0","id":3,"method":"session/prompt","params":{{"sessionId":"{session}","prompt":[{{"type":"text","text":"write it"}}]}}}}"#
            ),
            &answering,
        )
        .expect("answered");
    assert_eq!(
        answer["result"]["stopReason"], "end_turn",
        "a refused tool is not a cancelled turn: {answer}"
    );
}
