//! Telegram message drives a turn (offline). Boots Runtime, runs `poll_once`
//! with injected fetch (canned `getUpdates`, captured `sendMessage`).
//! Skips when guests not staged.

use std::cell::RefCell;

use jan_klod_core::Runtime;
use jan_klod_host::telegram::poll_once;

use crate::common;

#[test]
fn telegram_message_drives_a_turn_and_replies() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-telegram-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.yaml");
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
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    // Injected HTTP: getUpdates → one message, sendMessage captured.
    let sent: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let fetch = |_method: &str, url: &str, _headers: &[(&str, &str)], body: Option<&[u8]>| {
        if url.contains("getUpdates") {
            let updates = serde_json::json!({
                "ok": true,
                "result": [{
                    "update_id": 100,
                    "message": { "message_id": 1, "from": { "id": 123 }, "chat": { "id": 555 }, "text": "hello there" }
                }]
            });
            Ok(updates.to_string().into_bytes())
        } else {
            sent.borrow_mut()
                .push(String::from_utf8_lossy(body.unwrap()).to_string());
            Ok(br#"{"ok":true}"#.to_vec())
        }
    };

    let next = poll_once(&mut agent, &fetch, "TEST-TOKEN", 0, None).expect("poll succeeds");
    assert_eq!(next, 101, "offset advances past the handled update");

    let sent = sent.into_inner();
    assert_eq!(sent.len(), 1, "exactly one reply sent");
    assert!(
        sent[0].contains("\"chat_id\":555"),
        "reply targets the chat: {}",
        sent[0]
    );
    assert!(
        sent[0].contains("pong"),
        "reply carries the answer: {}",
        sent[0]
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Mock provider: requests write, then reports done (gate has something to confirm).
fn write_then_answer_http() -> jan_klod_core::route::HttpFn {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;
    let calls = Arc::new(AtomicU32::new(0));
    Box::new(move |_m, _u, _h, _b, _t| {
        let n = calls.fetch_add(1, Ordering::Relaxed);
        let body = if n == 0 {
            serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call-1",
                            "function": {
                                "name": "fs",
                                "arguments": r#"{"op":"write","path":"note.txt","contents":"hi"}"#
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
        } else {
            serde_json::json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "wrote note.txt" },
                    "finish_reason": "stop"
                }]
            })
        };
        Ok(jan_klod_core::http::WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}

/// Headless asks: confirmation as message, next message answers. Messages from
/// other chats mid-question are deferred, not dropped.
#[test]
fn a_confirmation_is_asked_in_the_chat_and_answered_by_the_next_message() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&[
        "provider-openai.wasm",
        "tool-fs.wasm",
        "interceptor-permission.wasm",
    ]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-tg-prompt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_confirmation_config(&dir);

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = write_then_answer_http;
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let sent: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let polls = std::cell::Cell::new(0);
    let fetch = |_method: &str, url: &str, _headers: &[(&str, &str)], body: Option<&[u8]>| {
        if url.contains("getUpdates") {
            let n = polls.get();
            polls.set(n + 1);
            // Poll 0: request. Poll 1 (blocked turn): "yes" + unrelated chat message.
            let result = if n == 0 {
                serde_json::json!([{
                    "update_id": 100,
                    "message": { "from": { "id": 123 }, "chat": { "id": 555 }, "text": "create note.txt" }
                }])
            } else if n == 1 {
                serde_json::json!([
                    { "update_id": 101, "message": { "from": { "id": 123 }, "chat": { "id": 555 }, "text": "yes" } },
                    { "update_id": 102, "message": { "from": { "id": 456 }, "chat": { "id": 777 }, "text": "hi" } }
                ])
            } else {
                serde_json::json!([])
            };
            Ok(serde_json::json!({ "ok": true, "result": result })
                .to_string()
                .into_bytes())
        } else {
            sent.borrow_mut()
                .push(String::from_utf8_lossy(body.unwrap()).to_string());
            Ok(br#"{"ok":true}"#.to_vec())
        }
    };

    let next = poll_once(&mut agent, &fetch, "TEST-TOKEN", 0, None).expect("poll succeeds");

    let sent = sent.into_inner();
    assert_confirmation_flow(&sent, next, &dir);
}

/// Telegram sender allowlist: unlisted sender is refused before session creation (#257).
/// Listed sender drives a turn normally. The principal is available for downstream.
#[test]
fn telegram_sender_allowlist_refuses_unlisted_and_accepts_listed() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-tg-allowlist-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.yaml");
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
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    // Allowlist only sender 999.
    let allowed_senders = vec![999i64];

    // Injected HTTP: getUpdates → two messages (one unlisted, one listed).
    let sent: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let fetch = |_method: &str, url: &str, _headers: &[(&str, &str)], body: Option<&[u8]>| {
        if url.contains("getUpdates") {
            let updates = serde_json::json!({
                "ok": true,
                "result": [
                    {
                        "update_id": 100,
                        "message": { "from": { "id": 888 }, "chat": { "id": 555 }, "text": "unlisted" }
                    },
                    {
                        "update_id": 101,
                        "message": { "from": { "id": 999 }, "chat": { "id": 666 }, "text": "hello" }
                    }
                ]
            });
            Ok(updates.to_string().into_bytes())
        } else {
            sent.borrow_mut()
                .push(String::from_utf8_lossy(body.unwrap()).to_string());
            Ok(br#"{"ok":true}"#.to_vec())
        }
    };

    let next = poll_once(&mut agent, &fetch, "TEST-TOKEN", 0, Some(&allowed_senders))
        .expect("poll succeeds");
    assert_eq!(next, 102, "offset advances past both updates");

    let sent = sent.into_inner();
    // Only the listed sender (999) should get a reply; unlisted sender (888) should not.
    assert_eq!(
        sent.len(),
        1,
        "exactly one reply sent (unlisted sender refused)"
    );
    assert!(
        sent[0].contains("\"chat_id\":666"),
        "reply targets the listed sender's chat: {}",
        sent[0]
    );
    assert!(
        sent[0].contains("pong"),
        "reply carries the answer: {}",
        sent[0]
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Config with provider, interceptors, tool.fs jailed to dir.
fn write_confirmation_config(dir: &std::path::Path) -> std::path::PathBuf {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
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
workspace: {}
",
            dir.display()
        ),
    )
    .unwrap();
    config
}

/// Verify confirmation flow: asked in right chat, standing options offered,
/// file+content named, write after reply, unrelated chat's message survived.
fn assert_confirmation_flow(sent: &[String], next: i64, dir: &std::path::Path) {
    let question = sent
        .iter()
        .find(|m| m.contains("Allow `fs`"))
        .unwrap_or_else(|| panic!("the user was asked before the write: {sent:?}"));
    assert!(
        question.contains("\"chat_id\":555"),
        "asked in the right chat: {question}"
    );
    assert!(
        question.contains("always"),
        "the standing options are offered: {question}"
    );
    // Chat surface: no terminal, question must carry whole decision (file + contents).
    assert!(
        question.contains("note.txt") && question.contains("hi"),
        "the question names the file and shows the content: {question}"
    );

    assert!(
        sent.iter().any(|m| m.contains("wrote note.txt")),
        "the turn finished after the reply: {sent:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.join("note.txt")).expect("the approved write happened"),
        "hi"
    );

    // Other chat's message deferred, not lost to offset advance.
    assert!(
        sent.iter().any(|m| m.contains("\"chat_id\":777")),
        "the unrelated chat still got a turn: {sent:?}"
    );
    assert_eq!(
        next, 103,
        "the offset covers every update consumed, answer-poll included"
    );
}
