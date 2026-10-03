//! A Python component answers a real tool call, through the real loop.
//!
//! `polyglot_ts.rs` proves a second language reaches the contracts. This
//! proves a third, and it proves one thing that one could not: the host
//! instantiates a component that imports **raw WASI**.
//!
//! # Why the imports matter here and did not there
//!
//! `CPython`'s standard library needs `wasi:filesystem`, `wasi:sockets` and
//! `wasi:cli` to initialise, and `componentize-py` has no flag to drop them —
//! where the `TypeScript` guest's `wasi:http` could be disabled and had to be,
//! since the host refuses to link it. These the host *does* link, through
//! `wasmtime_wasi::p2::add_to_linker_sync`, and denies at the context:
//! `WasiCtxBuilder::new()` configures no preopens and refuses every socket
//! address. `sandbox_boundary.rs` asserts those refusals against a guest that
//! reaches for them on purpose; this test asserts the other half, that a guest
//! which merely *imports* them still loads and works.
//!
//! # Why it skips rather than committing a fixture
//!
//! The component is **18.5 MB** — larger than the `TypeScript` guest's 12.7 MB,
//! because it carries `CPython` — so it is built where the toolchain exists and
//! skipped where it does not (#188). The toolchain itself is the cheap half at
//! 50 MB, against 326 MB for `jco`.
//!
//! `JK_REQUIRE_GUESTS=1` does **not** force this one, for the reason
//! `polyglot_ts.rs` gives: that flag means "the Rust guests must be staged".

use jan_klod_core::conductor::RunResult;
use jan_klod_core::Runtime;

use crate::common;

/// The component this test needs, which `make -C src/extensions py-guest`
/// stages when `componentize-py` is present.
const PY_COMPONENT: &str = "tool-hello-py.wasm";

#[test]
fn a_python_tool_answers_a_real_tool_call() {
    let ext_dir = common::repo_root().join("ext");
    if !ext_dir.join(PY_COMPONENT).exists() {
        eprintln!(
            "skipping: {PY_COMPONENT} is not staged — build it with \
             `make -C src/extensions py-guest` (needs componentize-py on PATH)"
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
    // `tool.hello-py` resolves to `ext/tool-hello-py.wasm` by the same naming
    // rule every other instance uses. That is the claim under test: the host
    // has no idea this one was written in Python.
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

    // First the model calls the tool, then it answers in words — the same two
    // completions `tool_wiring.rs` uses to drive a fleet dispatch.
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let factory = move || -> jan_klod_core::route::HttpFn {
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
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    };

    // Booting is itself an assertion: this component imports `wasi:sockets`
    // and `wasi:filesystem`, and a linker missing either would fail here the
    // way the TypeScript guest failed on `wasi:http` before it was disabled.
    let runtime = Runtime::boot(&config, &ext_dir).expect("the runtime boots with a Python guest");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let out = agent.run("py-1", "greet polyglot");
    assert!(
        matches!(out, RunResult::Answered { .. }),
        "the turn completed: {out:?}"
    );

    // The guest's own words, not the host's: `tool-hello-py` builds this
    // string, so finding it in the transcript means the component ran.
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
