//! Phase 4 Slice 4b — a Telegram message drives a turn, offline.
//!
//! Boots a `Runtime`, then runs one `poll_once` cycle with an **injected fetch**
//! that returns a canned `getUpdates` (one message) and captures the outbound
//! `sendMessage`. Proves the whole chat path — inbound message → loop (through the
//! sandboxed guests) → reply — with no network and no Telegram, no UI client.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::cell::RefCell;
use std::path::PathBuf;

use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::telegram::poll_once;
use jan_klod_core::Runtime;

fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

fn canned_provider_http(content: &'static str) -> HttpFn {
    Box::new(move |_m, _u, _h, _b, _t| {
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        });
        Ok(WireResponse { status: 200, headers: vec![], body: serde_json::to_vec(&body).unwrap() })
    })
}

#[test]
fn telegram_message_drives_a_turn_and_replies() {
    let ext_dir = repo_root().join("ext");
    for guest in ["provider-openai.wasm", "interceptor-intent-router.wasm"] {
        if !ext_dir.join(guest).exists() {
            eprintln!("skipping: {guest} not staged — run `make ext`");
            return;
        }
    }

    let dir = std::env::temp_dir().join(format!("jk-telegram-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "
extensions:
  store:
    memory:
      enabled: true
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
    let factory = || canned_provider_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    // Injected Telegram HTTP: getUpdates returns one message; sendMessage is captured.
    let sent: RefCell<Vec<String>> = RefCell::new(Vec::new());
    let fetch = |_method: &str, url: &str, _headers: &[(&str, &str)], body: Option<&[u8]>| {
        if url.contains("getUpdates") {
            let updates = serde_json::json!({
                "ok": true,
                "result": [{
                    "update_id": 100,
                    "message": { "message_id": 1, "chat": { "id": 555 }, "text": "hello there" }
                }]
            });
            Ok(updates.to_string().into_bytes())
        } else {
            sent.borrow_mut().push(String::from_utf8_lossy(body.unwrap()).to_string());
            Ok(br#"{"ok":true}"#.to_vec())
        }
    };

    let next = poll_once(&mut agent, &fetch, "TEST-TOKEN", 0).expect("poll succeeds");
    assert_eq!(next, 101, "offset advances past the handled update");

    let sent = sent.into_inner();
    assert_eq!(sent.len(), 1, "exactly one reply sent");
    assert!(sent[0].contains("\"chat_id\":555"), "reply targets the chat: {}", sent[0]);
    assert!(sent[0].contains("pong"), "reply carries the answer: {}", sent[0]);

    std::fs::remove_dir_all(&dir).ok();
}
