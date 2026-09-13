//! A session remembers what was said in it — a request must actually replay
//! prior turns to the model, not just persist them for the transcript endpoint.
//! These assert on what the provider wire actually received, since the store and
//! transcript endpoint can look correct while the model is still asked in
//! isolation.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::sync::{Arc, Mutex};

use jan_klod_core::conductor::RunResult;
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

use crate::common;

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
        Ok(WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&reply).unwrap(),
        })
    })
}

/// A config with a durable store, so a session survives a `Runtime` restart.
fn write_config(dir: &std::path::Path) -> std::path::PathBuf {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
storage:
  path: {}
extensions:
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
    assert!(matches!(
        agent.run("s-1", "read src/main.rs"),
        RunResult::Answered { .. }
    ));
    assert_eq!(
        last_messages(&seen),
        vec!["read src/main.rs"],
        "a first turn stands alone"
    );

    // Turn 2: the request must carry turn 1's question *and* its answer.
    assert!(matches!(
        agent.run("s-1", "now add a test for that"),
        RunResult::Answered { .. }
    ));
    assert_eq!(
        last_messages(&seen),
        vec!["read src/main.rs", "ok", "now add a test for that"],
        "the earlier exchange precedes the new message, oldest first"
    );

    // A different session is a different conversation.
    assert!(matches!(
        agent.run("s-2", "unrelated question"),
        RunResult::Answered { .. }
    ));
    assert_eq!(
        last_messages(&seen),
        vec!["unrelated question"],
        "sessions do not bleed"
    );
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

    // A fresh Runtime over the same SQLite file, as a restart of `jan-klod serve` would be.
    let recorded = Arc::clone(&seen);
    let factory = move || recording_http(&recorded);
    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime reboots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    agent.run("resumed", "what was I doing?");

    let messages = last_messages(&seen);
    assert!(
        messages
            .first()
            .is_some_and(|m| m.contains("widget refactor")),
        "the pre-restart turn is replayed: {messages:?}"
    );
    assert_eq!(
        messages.last().map(String::as_str),
        Some("what was I doing?")
    );
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
    // 20 replayed turns × (question + answer) + the new message: replay is
    // capped so a long session doesn't reload its whole history every turn.
    assert!(
        messages.len() <= 41,
        "replay is capped: {} messages",
        messages.len()
    );
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
storage:
  path: {}
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
    assert_eq!(
        last_messages(&seen).first().map(String::as_str),
        Some("you are a test agent")
    );

    // A second turn must not accumulate a second copy.
    agent.run("sys-1", "now also update the docs for it");
    let roles = last_roles(&seen);
    assert_eq!(
        roles.iter().filter(|r| *r == "system").count(),
        1,
        "exactly one system message per request: {roles:?}"
    );
    assert_eq!(
        roles.first().map(String::as_str),
        Some("system"),
        "still first: {roles:?}"
    );
}

/// Config with the tool fleet and the interceptors that advertise it, so the
/// assembled request is the one a shipped install sends.
fn write_config_with_tools(dir: &std::path::Path) -> std::path::PathBuf {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
storage:
  path: {db}
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

/// The whole chain from the fleet to the wire, checked by reading tool schemas
/// back off the request body. Other tool-use tests use a canned provider that
/// returns `tool_calls` regardless of what it was sent, so they wouldn't catch a
/// request that carried no tool schemas at all — a real model would just never
/// call the tool.
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
    assert!(
        names.contains(&"fs"),
        "the fs tool is advertised: {names:?}"
    );
    assert!(
        names.contains(&"find"),
        "the find tool is advertised: {names:?}"
    );

    // A name alone isn't enough — the model needs the argument schema to build a call.
    let find = tools
        .iter()
        .find(|t| t["function"]["name"] == "find")
        .expect("find is present");
    let params = &find["function"]["parameters"];
    assert_eq!(
        params["type"], "object",
        "a real JSON Schema, not a placeholder: {params}"
    );
    assert!(
        params["properties"]["pattern"].is_object(),
        "the schema names `find`'s required argument: {params}"
    );
}

/// Collects the warnings a turn streams, the way the REST surface and TUI do.
#[derive(Default)]
struct WarningSink(Vec<String>);

impl jan_klod_core::conductor::EventSink for WarningSink {
    fn emit(&mut self, event: &jan_klod_core::conductor::Event) -> jan_klod_core::conductor::Flow {
        if let jan_klod_core::conductor::Event::Warning(message) = event {
            self.0.push(message.clone());
        }
        jan_klod_core::conductor::Flow::Continue
    }
}

/// A truncated completion (`finish_reason: "length"`) reaches the user as a
/// warning, not silently indistinguishable from a completed answer.
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

    // A reply the model ran out of room on, as an endpoint would report it.
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
    let mut sink = WarningSink::default();
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

/// `AGENTS.md` in the workspace reaches the model, labelled as the project's
/// (docs claim jan-klod reads it; this proves the claim, since it once didn't).
///
/// Read host-side and passed to `interceptor-system` as config, rather than
/// granting interceptors `host-fs` just to read one file — that would widen
/// file access to every decision component for no reason.
#[test]
fn project_instructions_from_agents_md_reach_the_model() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-system.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-agentsmd-{}", std::process::id()));
    let work = dir.join("repo");
    std::fs::create_dir_all(&work).unwrap();
    let _guard = common::TempDir(dir.clone());
    std::fs::write(
        work.join("AGENTS.md"),
        "Run `cargo nextest run`, never `cargo test`.\nNever touch `vendor/`.\n",
    )
    .unwrap();

    let config = dir.join("config.yaml");
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
    system:
      enabled: true
",
            work.display()
        ),
    )
    .unwrap();

    let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = std::sync::Arc::clone(&bodies);
    let factory = move || -> jan_klod_core::route::HttpFn {
        let seen = std::sync::Arc::clone(&seen);
        Box::new(move |_m, _u, _h, body, _t| {
            if let Some(bytes) = body {
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(bytes).into_owned());
            }
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&serde_json::json!({
                    "choices": [{ "message": { "role": "assistant", "content": "ok" },
                                  "finish_reason": "stop" }]
                }))
                .unwrap(),
            })
        })
    };

    let runtime =
        jan_klod_core::Runtime::boot(&config, common::repo_root().join("ext")).expect("boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let _ = agent.run("s", "hello");

    let requests = bodies.lock().unwrap().join("\n");
    assert!(
        requests.contains("cargo nextest run"),
        "the project's own instructions reach the model: {requests}"
    );
    // Labelled: the standing prompt describes what the runtime enforces, while
    // AGENTS.md is just a request from the repo — conflating them would let
    // AGENTS.md claim authority over the sandbox it doesn't have.
    assert!(
        requests.contains("from AGENTS.md"),
        "and are marked as the project's rather than the runtime's: {requests}"
    );
    assert!(
        requests.contains("cannot grant permissions the sandbox refuses"),
        "with their authority stated: {requests}"
    );
}
