//! Thin-loop exit test — boots the real `Runtime` from a `config.yaml` and runs
//! turns through the core conductor (Slice 2c), the way Phase 2's exit gate will.
//!
//! Unlike `routing.rs` (the retired v0 `manager-agent-loop` path), this drives
//! `Runtime::build_agent` → `AgentSession::run`: the enabled `interceptor.*`
//! become the dispatcher and the enabled `provider.*` the completer fallback
//! chain. The provider's `host-http` is canned, so the whole loop runs with no
//! network and no API key.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`; build them
//! with `make ext`.

use jan_klod_core::conductor::RunResult;
use jan_klod_core::Runtime;

mod common;

#[test]
fn thin_loop_runs_turns_from_config() {
    let ext_dir = common::repo_root().join("ext");
    for file in ["provider-openai.wasm", "interceptor-intent-router.wasm"] {
        if !ext_dir.join(file).exists() {
            eprintln!("skipping: {file} not staged — run `make ext`");
            return;
        }
    }

    let dir = std::env::temp_dir().join(format!("jk-thinloop-{}", std::process::id()));
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
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    // A greeting: the intent router's heuristic tier classifies it `simple` and
    // blocks the agentic loop; the core still answers inline via the provider.
    let greeting = agent.run("session-1", "hello");
    assert_eq!(
        greeting,
        RunResult::Answered { text: "pong".into(), agentic: false },
        "a greeting short-circuits the agentic loop"
    );

    // A multi-step English prompt passes the heuristics to the LLM classifier
    // tier (safe-default `agentic` in v1) → the shaping + ReAct path runs.
    let task = agent.run("session-1", "Refactor the auth module and run the whole test suite");
    assert_eq!(
        task,
        RunResult::Answered { text: "pong".into(), agentic: true },
        "a multi-step prompt runs the agentic path"
    );

    std::fs::remove_dir_all(&dir).ok();
}
