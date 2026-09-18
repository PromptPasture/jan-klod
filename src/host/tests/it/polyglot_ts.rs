//! A TypeScript component answers a real tool call, through the real loop.
//!
//! `polyglot.rs` proves the Component Model boundary is language-neutral by
//! instantiating a committed `TinyGo` fixture directly. This proves something
//! narrower and more current: a guest written in a second language, built
//! today against today's `wit/`, is dispatched by the tool fleet during an
//! ordinary turn — no host-side special case, no separate path.
//!
//! # Why it skips rather than committing a fixture
//!
//! The `TinyGo` spike is a committed artifact at ~75 KB. The TypeScript
//! component is **12.7 MB**, because every JavaScript component carries an
//! engine, so it is built where the toolchain exists and skipped where it does
//! not (#187). The skip is loud: it names what is missing and how to get it,
//! which is what stops "nothing ran" from reading like "everything passed".
//!
//! `JK_REQUIRE_GUESTS=1` does **not** force this one. That flag means "the
//! Rust guests must be staged"; a 326 MB toolchain is a different ask, and
//! conflating them would make `make gate` fail on every machine that has not
//! opted in.

use jan_klod_core::conductor::RunResult;
use jan_klod_core::Runtime;

use crate::common;

/// The component this test needs, which `make -C src/extensions ts-guest`
/// stages when `jco` is present.
const TS_COMPONENT: &str = "tool-hello-ts.wasm";

#[test]
fn a_typescript_tool_answers_a_real_tool_call() {
    let ext_dir = common::repo_root().join("ext");
    if !ext_dir.join(TS_COMPONENT).exists() {
        eprintln!(
            "skipping: {TS_COMPONENT} is not staged — build it with \
             `make -C src/extensions ts-guest` (needs jco on PATH)"
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
    // `tool.hello-ts` resolves to `ext/tool-hello-ts.wasm` by the same naming
    // rule every other instance uses. That is the claim under test: the host
    // has no idea this one was written in TypeScript.
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

    // First the model calls the tool, then it answers in words — the same two
    // completions `tool_wiring.rs` uses to drive a fleet dispatch.
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let factory = move || -> jan_klod_core::route::HttpFn {
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
            Ok(jan_klod_core::http::WireResponse {
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

    // The guest's own words, not the host's: `tool-hello-ts` builds this
    // string, so finding it in the transcript means the component ran.
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
