//! Exit-gate test — the whole Phase 1 flow in one driven turn, offline.
//!
//! Where `component_harness` verifies each guest's interface in isolation, this
//! drives the [`jan_klod_host::agent`] loop end to end: config-driven load →
//! lifecycle → provider completion → store persistence, all over the Component
//! Model. The provider's `host-http` is a canned `OpenAI` Chat Completions reply,
//! so the gate runs with no network and no API key — the same `run_turn` the
//! `jan-klod --turn` binary uses live.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`, so a bare
//! `cargo test` stays green; `make harness` stages them first.

use std::path::{Path, PathBuf};

use jan_klod_core::http::WireResponse;
use jan_klod_host::agent::{self, HttpFn};

/// Repo root, resolved from this crate's manifest dir (`src/core/host`).
fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

/// True when both first-party guests are staged — otherwise the gate skips.
fn guests_staged(ext_dir: &Path) -> bool {
    ext_dir.join("provider-openai.wasm").exists() && ext_dir.join("store-memory.wasm").exists()
}

/// A canned HTTP backend returning a fixed `OpenAI` Chat Completions body, so the
/// provider completes deterministically with no network.
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
fn agent_turn_completes_and_persists() {
    let root = repo_root();
    let ext_dir = root.join("ext");
    if !guests_staged(&ext_dir) {
        eprintln!(
            "skipping: guests not staged in {} — run `make extensions` (or `make harness`)",
            ext_dir.display()
        );
        return;
    }

    // Minimal config: the enabled store + an enabled OpenAI-compatible provider.
    // base-url/model/api-key are literal (the canned backend ignores them, but
    // the provider reads them from host-config).
    let config_dir = std::env::temp_dir().join(format!("jk-exit-gate-{}", std::process::id()));
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("jan-klod.yaml");
    std::fs::write(
        &config_path,
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
",
    )
    .unwrap();

    let turn = agent::run_turn(&config_path, &ext_dir, "ping", canned_http("pong"))
        .expect("agent turn should complete");

    // Completion leg: the canned body parses to one text delta closed by `stop`.
    assert_eq!(turn.response, "pong");
    assert_eq!(turn.done_reason, "stop");

    // Persistence leg: the value read back from the store carries both sides of
    // the turn, proving config → completion → store all flowed over the CM.
    assert_eq!(turn.namespace, "agent.history");
    assert!(!turn.stored_id.is_empty(), "the store assigned a row id");
    let stored: serde_json::Value = serde_json::from_str(&turn.stored_value)
        .expect("stored value is the JSON the turn wrote");
    assert_eq!(stored["prompt"], "ping");
    assert_eq!(stored["response"], "pong");

    std::fs::remove_dir_all(&config_dir).ok();
}
