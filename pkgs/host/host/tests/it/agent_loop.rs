//! Runs turns through the real `Runtime` with canned HTTP (no network or API
//! key). Skips when guests are not staged in `ext/`; build with `make ext`.

use jk_agent::conductor::RunResult;
use jk_agent::Runtime;

use crate::common;

#[test]
fn thin_loop_runs_turns_from_config() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-thinloop-{}", std::process::id()));
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

    let greeting = agent.run("session-1", "hello");
    assert_eq!(
        greeting,
        RunResult::Answered {
            text: "pong".into(),
            agentic: false
        },
        "a greeting short-circuits the agentic loop"
    );

    let task = agent.run(
        "session-1",
        "Refactor the auth module and run the whole test suite",
    );
    assert_eq!(
        task,
        RunResult::Answered {
            text: "pong".into(),
            agentic: true
        },
        "a multi-step prompt runs the agentic path"
    );

    std::fs::remove_dir_all(&dir).ok();
}
