//! A TypeScript component, built against today's `wit/`, answers a real tool
//! call in an ordinary turn. It skips when not staged (#187): the component is
//! 12.7 MB and needs `jco`. `JK_REQUIRE_GUESTS` does not force it.

use jk_agent::conductor::RunResult;
use jk_agent::Runtime;

use crate::common;

/// Staged by `make -C pkgs/extensions ts-guest`.
const TS_COMPONENT: &str = "tool-hello-ts.wasm";

#[test]
fn a_typescript_tool_answers_a_real_tool_call() {
    let ext_dir = common::repo_root().join("ext");
    if !ext_dir.join(TS_COMPONENT).exists() {
        eprintln!(
            "skipping: {TS_COMPONENT} is not staged — build it with \
             `make -C pkgs/extensions ts-guest` (needs jco on PATH)"
        );
        return;
    }
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-tool-selector.wasm"]) {
        return;
    }

    let dir = common::TempDir(
        std::env::temp_dir().join(format!("jk-polyglot-ts-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let config = dir.0.join("config.yaml");
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
  tool:
    hello-ts:
      enabled: true
  interceptor:
    tool-selector:
      enabled: true
",
    )
    .expect("writes the config");

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let factory = move || -> jk_wasm::route::HttpFn {
        let calls = std::sync::Arc::clone(&calls);
        Box::new(move |_m, _u, _h, _b, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"hello-ts",
                     "arguments":"{\"name\":\"polyglot\"}"}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"the tool answered"},"finish_reason":"stop"}]})
            };
            Ok(jk_wasm::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    };

    let runtime = Runtime::boot(&config, &ext_dir).expect("the runtime boots with a TS guest");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let out = agent.run("ts-1", "greet polyglot");
    assert!(
        matches!(out, RunResult::Answered { .. }),
        "the turn completed: {out:?}"
    );

    let recorded = agent
        .transcript("ts-1")
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        recorded.contains("hello, polyglot, from TypeScript"),
        "the TypeScript guest's result is not in the transcript: {recorded}"
    );
}
