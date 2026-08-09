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

/// A config that also enables the system-prompt interceptor, so the assembled
/// request is the one a shipped install produces.
fn write_config_with_system(dir: &std::path::Path, prompt: &str) -> std::path::PathBuf {
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
  interceptor:
    intent-router:
      enabled: true
    system:
      enabled: true
{prompt}
",
            dir.join("jan-klod.db").display()
        ),
    )
    .unwrap();
    config
}

/// The `role` of every message in the last recorded request.
fn last_roles(seen: &Arc<Mutex<Vec<serde_json::Value>>>) -> Vec<String> {
    let recorded = seen.lock().unwrap();
    let last = recorded.last().expect("the provider was called").clone();
    drop(recorded);
    last["messages"]
        .as_array()
        .expect("a messages array")
        .iter()
        .map(|m| m["role"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn the_model_is_told_what_it_is_before_anything_else() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-system.wasm"]) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-system-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config_with_system(&dir, "      prompt: \"you are a test agent\"");

    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let factory = move || recording_http(&recorded);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    // A prompt the intent router sends down the agentic path, where request
    // shaping (and therefore this interceptor) runs.
    agent.run("sys-1", "read src/main.rs and then add a test for it");
    assert_eq!(
        last_roles(&seen).first().map(String::as_str),
        Some("system"),
        "the instructions come first: {:?}",
        last_messages(&seen)
    );
    assert_eq!(last_messages(&seen).first().map(String::as_str), Some("you are a test agent"));

    // A second turn must not accumulate a second copy.
    agent.run("sys-1", "now also update the docs for it");
    let roles = last_roles(&seen);
    assert_eq!(
        roles.iter().filter(|r| *r == "system").count(),
        1,
        "exactly one system message per request: {roles:?}"
    );
    assert_eq!(roles.first().map(String::as_str), Some("system"), "still first: {roles:?}");
}

/// Config with the tool fleet and the interceptors that advertise it, so the
/// assembled request is the one a shipped install sends.
fn write_config_with_tools(dir: &std::path::Path) -> std::path::PathBuf {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
extensions:
  store:
    sqlite:
      enabled: true
      path: {db}
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    intent-router:
      enabled: true
    tool-selector:
      enabled: true
  tool:
    fs:
      enabled: true
    find:
      enabled: true
workspace: {ws}
",
            db = dir.join("jan-klod.db").display(),
            ws = dir.display()
        ),
    )
    .unwrap();
    config
}

/// The whole chain from the fleet to the wire is only observable here.
///
/// Every offline test of tool use works with a canned provider that returns
/// `tool_calls` regardless of what it was sent — so a request that carried no
/// tool schemas at all would still drive a green `ReAct` test, while a real model,
/// never having been told the tools exist, would answer in prose forever. This
/// reads the schemas back off the request body.
#[test]
fn the_model_is_actually_told_which_tools_exist() {
    if !common::guests_staged(&[
        "provider-openai.wasm",
        "interceptor-intent-router.wasm",
        "interceptor-tool-selector.wasm",
        "tool-fs.wasm",
        "tool-find.wasm",
    ]) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-tools-wire-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config_with_tools(&dir);

    let seen: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&seen);
    let factory = move || recording_http(&recorded);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    agent.run("tools-1", "find the config file and then read it");

    let recorded = seen.lock().unwrap();
    let body = recorded.last().expect("the provider was called").clone();
    drop(recorded);

    let tools = body["tools"].as_array().unwrap_or_else(|| {
        panic!("the request carries a `tools` array; body was {body}");
    });
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t["function"]["name"].as_str())
        .collect();
    assert!(names.contains(&"fs"), "the fs tool is advertised: {names:?}");
    assert!(names.contains(&"find"), "the find tool is advertised: {names:?}");

    // A name alone is not usable — the model needs the argument schema to build a
    // call, and an empty `{}` here would look fine while making every call a guess.
    let find = tools
        .iter()
        .find(|t| t["function"]["name"] == "find")
        .expect("find is present");
    let params = &find["function"]["parameters"];
    assert_eq!(params["type"], "object", "a real JSON Schema, not a placeholder: {params}");
    assert!(
        params["properties"]["pattern"].is_object(),
        "the schema names `find`'s required argument: {params}"
    );
}

/// A truncated completion reaches the user as a warning, not as a full stop.
///
/// `finish_reason: "length"` was parsed by the provider guest and then thrown
/// away when the host drained the chunk stream, so an answer the model cut off
/// mid-sentence was indistinguishable from one it finished — the kind of wrong a
/// user acts on. This drives the whole path with a canned reply that stops at the
/// limit.
#[test]
fn a_truncated_answer_is_flagged_to_the_client() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-truncated-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config(&dir);

    // A reply the model ran out of room on, exactly as an endpoint reports it.
    let http = || -> HttpFn {
        Box::new(move |_m, _u, _h, _b, _t| {
            let body = serde_json::json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "the first half of the ans" },
                    "finish_reason": "length"
                }]
            });
            Ok(WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    };

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime.build_agent(&http).expect("agent boots");

    // Collect the streamed events the way the REST surface and TUI do.
    #[derive(Default)]
    struct Collect(Vec<String>);
    impl jan_klod_core::conductor::EventSink for Collect {
        fn emit(&mut self, event: &jan_klod_core::conductor::Event) -> jan_klod_core::conductor::Flow {
            if let jan_klod_core::conductor::Event::Warning(message) = event {
                self.0.push(message.clone());
            }
            jan_klod_core::conductor::Flow::Continue
        }
    }
    let mut sink = Collect::default();
    let out = agent.run_streaming_headless(&mut sink, "trunc-1", "explain everything");

    assert!(
        sink.0.iter().any(|w| w.contains("cut off")),
        "the client is told the answer is incomplete: {:?}",
        sink.0
    );
    // The partial text is still the answer — a cut-off reply beats no reply.
    assert!(
        matches!(&out, RunResult::Answered { text, .. } if text.contains("first half")),
        "the partial answer still comes back: {out:?}"
    );
}
