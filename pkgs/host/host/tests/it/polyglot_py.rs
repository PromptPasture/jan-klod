//! A Python component answers a real tool call; it imports raw WASI that the host links and denies at the context.
//! Skips when not staged (#188): it is 18.5 MB and needs `componentize-py`. `JK_REQUIRE_GUESTS` does not force it.

use jk_agent::conductor::RunResult;
use jk_agent::Runtime;

use crate::common;

/// The component this test needs, which `make -C pkgs/extensions py-guest`
/// stages when `componentize-py` is present.
const PY_COMPONENT: &str = "tool-hello-py.wasm";

#[test]
fn a_python_tool_answers_a_real_tool_call() {
    let ext_dir = common::repo_root().join("ext");
    if !ext_dir.join(PY_COMPONENT).exists() {
        eprintln!(
            "skipping: {PY_COMPONENT} is not staged — build it with \
             `make -C pkgs/extensions py-guest` (needs componentize-py on PATH)"
        );
        return;
    }
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-tool-selector.wasm"]) {
        return;
    }

    let dir = common::TempDir(
        std::env::temp_dir().join(format!("jk-polyglot-py-{}", std::process::id())),
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
    hello-py:
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
                    {"id":"c1","function":{"name":"hello-py",
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

    let runtime = Runtime::boot(&config, &ext_dir).expect("the runtime boots with a Python guest");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let out = agent.run("py-1", "greet polyglot");
    assert!(
        matches!(out, RunResult::Answered { .. }),
        "the turn completed: {out:?}"
    );

    let recorded = agent
        .transcript("py-1")
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        recorded.contains("hello, polyglot, from Python"),
        "the Python guest's result is not in the transcript: {recorded}"
    );
}
