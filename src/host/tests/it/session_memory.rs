//! Sessions must replay prior turns to the model, not just persist them.
//! Asserts on what the provider wire received (store/transcript can look
//! correct while model still sees isolation). Skips when guests not staged.

use std::sync::{Arc, Mutex};

use jan_klod_core::conductor::RunResult;
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

use crate::common;

const GUESTS: [&str; 1] = ["provider-openai.wasm"];

/// Record every request to read the message list the model saw.
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

/// Config with durable store (sessions survive Runtime restart).
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

/// Extract message `content` from the last recorded request.
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

    // Turn 1: stands alone.
    assert!(matches!(
        agent.run("s-1", "read src/main.rs"),
        RunResult::Answered { .. }
    ));
    assert_eq!(
        last_messages(&seen),
        vec!["read src/main.rs"],
        "a first turn stands alone"
    );

    // Turn 2: carries prior exchange (oldest first).
    assert!(matches!(
        agent.run("s-1", "now add a test for that"),
        RunResult::Answered { .. }
    ));
    assert_eq!(
        last_messages(&seen),
        vec!["read src/main.rs", "ok", "now add a test for that"],
        "the earlier exchange precedes the new message, oldest first"
    );

    // Different session = different conversation.
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

    // Restart: fresh Runtime over same SQLite file.
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
    // ≤41 messages: 20 replayed turns × (Q+A) + new message (capped, not full history).
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

/// Config with system-prompt interceptor (like shipped install).
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

/// Extract message `role` from the last recorded request.
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

    // Intent router sends prompt down agentic path (where this interceptor runs).
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

    // Second turn must not duplicate the system message.
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

/// Config with tool fleet and interceptors advertising it (like shipped).
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

/// Tool schemas on the wire (caught by reading request body). Canned providers
/// return `tool_calls` blindly, missing requests with no schemas.
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

    // Name alone isn't enough; model needs the argument schema.
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

/// Collect warnings like REST surface and TUI do.
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

/// Truncated completion (`finish_reason: "length"`) is flagged as warning, not silent.
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

    // Model ran out of room (like an endpoint reports).
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

    // Collect streamed events like REST surface and TUI do.
    let mut sink = WarningSink::default();
    let out = agent.run_streaming_headless(&mut sink, "trunc-1", "explain everything");

    assert!(
        sink.0.iter().any(|w| w.contains("cut off")),
        "the client is told the answer is incomplete: {:?}",
        sink.0
    );
    // Partial text is still the answer (cut-off reply better than no reply).
    assert!(
        matches!(&out, RunResult::Answered { text, .. } if text.contains("first half")),
        "the partial answer still comes back: {out:?}"
    );
}

/// AGENTS.md reaches the model labelled as the project's instructions.
/// Read host-side, passed to `interceptor-system` as config (not granting
/// `host-fs` to every interceptor just to read one file).
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
    // Labelled: standing prompt vs project request; AGENTS.md can't claim sandbox authority.
    assert!(
        requests.contains("from AGENTS.md"),
        "and are marked as the project's rather than the runtime's: {requests}"
    );
    assert!(
        requests.contains("cannot grant permissions the sandbox refuses"),
        "with their authority stated: {requests}"
    );
}
