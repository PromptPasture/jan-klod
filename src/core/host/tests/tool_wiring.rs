//! Phase 8 Slice 8a — `build_agent` instantiates enabled `tool.*` into the fleet.
//!
//! Boots a `Runtime` from a config that enables `tool.fs-probe` and a workspace,
//! and asserts the built `AgentSession` carries that tool — proving the config →
//! capability → fleet wiring (the loop can now reach real tools). Offline.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::path::PathBuf;

use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

fn canned_http(content: &'static str) -> HttpFn {
    Box::new(move |_m, _u, _h, _b, _t| {
        let body = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": content }, "finish_reason": "stop" }]
        });
        Ok(WireResponse { status: 200, headers: vec![], body: serde_json::to_vec(&body).unwrap() })
    })
}

#[test]
fn build_agent_wires_enabled_tools_into_the_fleet() {
    let ext_dir = repo_root().join("ext");
    for guest in ["provider-openai.wasm", "interceptor-intent-router.wasm", "tool-fs-probe.wasm"] {
        if !ext_dir.join(guest).exists() {
            eprintln!("skipping: {guest} not staged — run `make ext`");
            return;
        }
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
  tool:
    fs-probe:
      enabled: true
workspace: {ws}
",
            ws = workspace.display()
        ),
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = || canned_http("ok");
    let agent = runtime.build_agent(&factory).expect("agent boots with tools");

    assert!(
        agent.tool_names().contains(&"fs-probe".to_string()),
        "the enabled tool.fs-probe should be in the fleet: {:?}",
        agent.tool_names()
    );

    std::fs::remove_dir_all(&dir).ok();
}
