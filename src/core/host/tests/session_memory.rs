//! A session remembers what was said in it.
//!
//! `run_turn` assembled each request from the current user message alone. The
//! transcript was persisted — `GET /session/:id` returned it, and it survived a
//! restart — but it was never read back into a request, so the model saw one
//! message per turn and a session had no memory at all. "Now add a test for that"
//! reached a model that had never seen "that", which is most of what a coding
//! agent is asked to do.
//!
//! These assert on **what the provider actually received**, captured from the
//! wire, because that is the only place the difference shows: everything else
//! (the store, the transcript endpoint, resume-by-id) looked correct while the
//! model was being asked in isolation.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::sync::{Arc, Mutex};

use jan_klod_core::conductor::RunResult;
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

mod common;

const GUESTS: [&str; 1] = ["provider-openai.wasm"];

/// Records every request body the provider was sent, so a test can read the
/// message list the model actually got.
fn recording_http(seen: &Arc<Mutex<Vec<serde_json::Value>>>) -> HttpFn {
    let seen = Arc::clone(seen);
    Box::new(move |_m, _u, _h, body: Option<&[u8]>, _t| {
        if let Some(bytes) = body {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes) {
                seen.lock().unwrap().push(value);
            }
        }
        let reply = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": "ok" },
                "finish_reason": "stop"
            }]
        });
        Ok(WireResponse { status: 200, headers: vec![], body: serde_json::to_vec(&reply).unwrap() })
    })
}

/// A config with a durable store, so a session survives a `Runtime` restart.
fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
extensions:
  store:
    sqlite:
      enabled: true
      path: {}
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
",
            dir.join("jan-klod.db").display()
        ),
    )
    .unwrap();
    config
}

/// The `content` of every message in the last recorded request.
fn last_messages(seen: &Arc<Mutex<Vec<serde_json::Value>>>) -> Vec<String> {
    let recorded = seen.lock().unwrap();
    let last = recorded.last().expect("the provider was called").clone();
    drop(recorded);
    last["messages"]
        .as_array()
        .expect("a messages array")
        .iter()
        .map(|m| m["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn a_later_turn_carries_what_was_said_earlier() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-memory-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config(&dir);

    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let factory = move || recording_http(&recorded);

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    // Turn 1: nothing to remember yet.
    assert!(matches!(agent.run("s-1", "read src/main.rs"), RunResult::Answered { .. }));
    assert_eq!(last_messages(&seen), vec!["read src/main.rs"], "a first turn stands alone");

    // Turn 2: the request must carry turn 1's question *and* its answer.
    assert!(matches!(agent.run("s-1", "now add a test for that"), RunResult::Answered { .. }));
    assert_eq!(
        last_messages(&seen),
        vec!["read src/main.rs", "ok", "now add a test for that"],
        "the earlier exchange precedes the new message, oldest first"
    );

    // A different session is a different conversation.
    assert!(matches!(agent.run("s-2", "unrelated question"), RunResult::Answered { .. }));
    assert_eq!(last_messages(&seen), vec!["unrelated question"], "sessions do not bleed");
}

#[test]
fn resuming_a_session_after_a_restart_carries_its_history() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-memory-restart-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config(&dir);

    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));

    {
        let recorded = Arc::clone(&seen);
        let factory = move || recording_http(&recorded);
        let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
        let mut agent = runtime.build_agent(&factory).expect("agent boots");
        agent.run("resumed", "remember the widget refactor");
    }

    // A fresh Runtime over the same SQLite file — what `jan-klod serve` does
    // after a restart, and what "resumable by id" has to mean to be worth having.
    let recorded = Arc::clone(&seen);
    let factory = move || recording_http(&recorded);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime reboots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    agent.run("resumed", "what was I doing?");

    let messages = last_messages(&seen);
    assert!(
        messages.first().is_some_and(|m| m.contains("widget refactor")),
        "the pre-restart turn is replayed: {messages:?}"
    );
    assert_eq!(messages.last().map(String::as_str), Some("what was I doing?"));
}

#[test]
fn replay_is_bounded_so_a_long_session_does_not_grow_without_limit() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-memory-bound-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config(&dir);

    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let factory = move || recording_http(&recorded);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    for i in 0..30 {
        agent.run("long", &format!("message {i}"));
    }

    let messages = last_messages(&seen);
    // 20 replayed turns × (question + answer) + the new message. The cap is what
    // stops turn 500 from loading five hundred turns out of SQLite every time.
    assert!(messages.len() <= 41, "replay is capped: {} messages", messages.len());
    assert!(
        !messages.iter().any(|m| m == "message 0"),
        "the oldest turns fall out of the window: {messages:?}"
    );
    assert!(
        messages.iter().any(|m| m == "message 29"),
        "the most recent turns are kept: {messages:?}"
    );
}
