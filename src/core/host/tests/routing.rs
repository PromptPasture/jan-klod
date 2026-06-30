//! Routing exit-gate test — Phase 1's exit gate, now driven entirely through
//! sandboxes with **core only brokering**.
//!
//! Where `component_harness` verifies each guest in isolation, this boots the
//! real runtime against a `jan-klod.yaml`, asks it to route the agent loop, and
//! runs one turn. The `manager-agent-loop` guest imports `llm-provider` and
//! `memory-store`; the core's routing layer satisfies those imports by delegating
//! into the provider and store extensions. The provider's `host-http` is a canned
//! reply, so the whole loop runs with no network and no API key — exactly the
//! exit gate (config -> completion -> store over the Component Model), but with
//! the loop logic where architecture.md mandates it: in the guest.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`; build them
//! with `make harness`.

use std::path::PathBuf;

use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

/// Repo root, resolved from this crate's manifest dir (`src/core/host`).
fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

/// A canned chat-completions reply, so the routed provider completes with no
/// network.
fn canned_http(content: &'static str) -> HttpFn {
    Box::new(move |_method, _url, _headers, _body, _timeout| {
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": content },
                "finish_reason": "stop"
            }]
        });
        Ok(WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}

#[test]
fn routed_agent_loop_runs_one_turn() {
    let ext_dir = repo_root().join("ext");
    for file in [
        "manager-agent-loop.wasm",
        "provider-openai.wasm",
        "store-memory.wasm",
    ] {
        if !ext_dir.join(file).exists() {
            eprintln!("skipping: {file} not staged — run `make harness`");
            return;
        }
    }

    // Minimal config: the manager loop, an OpenAI-compatible provider, and the
    // in-memory store — the three the loop routes across.
    let dir = std::env::temp_dir().join(format!("jk-routing-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("jan-klod.yaml");
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
  manager:
    agent-loop:
      enabled: true
",
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime
        .route_agent_loop(canned_http("pong"))
        .expect("agent loop routes provider + store");

    // run() completes through the routed provider, persists through the routed
    // store, and (inside the guest) reads it back — so a clean "pong" proves both
    // legs crossed the component boundary via core's broker.
    let text = agent.run("ping").expect("routed turn succeeds");
    assert_eq!(text, "pong");

    std::fs::remove_dir_all(&dir).ok();
}
