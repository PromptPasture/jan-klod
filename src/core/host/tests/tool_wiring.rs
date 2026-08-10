//! Phase 8 Slice 8a — `build_agent` instantiates enabled `tool.*` into the fleet.
//!
//! Boots a `Runtime` from a config that enables `tool.fs` and a workspace,
//! and asserts the built `AgentSession` carries that tool — proving the config →
//! capability → fleet wiring (the loop can now reach real tools). Offline.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use jan_klod_core::Runtime;

mod common;

#[test]
fn build_agent_wires_enabled_tools_into_the_fleet() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm", "tool-fs.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-toolwire-{}", std::process::id()));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
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
    intent-router:
      enabled: true
  tool:
    fs:
      enabled: true
workspace: {ws}
",
            ws = workspace.display()
        ),
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || common::canned_http("ok");
    let agent = runtime.build_agent(&factory).expect("agent boots with tools");

    assert!(
        agent.tool_names().contains(&"fs".to_string()),
        "the enabled tool.fs should be in the fleet: {:?}",
        agent.tool_names()
    );

    std::fs::remove_dir_all(&dir).ok();
}
