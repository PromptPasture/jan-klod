//! The `providers:` chain sets fallback order. Each provider answers with
//! distinct text, so the order is observable.

use jk_agent::conductor::RunResult;
use jk_agent::Runtime;
use jk_wasm::http::WireResponse;
use jk_wasm::route::HttpFn;

use crate::common;

/// Mock responds as the provider calling (identified by `base-url`).
fn per_provider_http() -> HttpFn {
    Box::new(move |_m, url: &str, _h, _b, _t| {
        let who = if url.contains("alpha") {
            "from-alpha"
        } else {
            "from-beta"
        };
        let body = serde_json::json!({
            "choices": [{
                "message": { "role": "assistant", "content": who },
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

/// Config with two enabled providers and a `providers:` chain.
fn config_with_chain(dir: &std::path::Path, chain: &str) -> std::path::PathBuf {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
extensions:
  provider:
    alpha:
      enabled: true
      type: openai
      base-url: http://alpha/v1
      model: mock-1
      api-key: test
    beta:
      enabled: true
      type: openai
      base-url: http://beta/v1
      model: mock-1
      api-key: test
{chain}
"
        ),
    )
    .unwrap();
    config
}

fn answer(config: &std::path::Path, ext_dir: &std::path::Path) -> String {
    let runtime = Runtime::boot(config, ext_dir).expect("runtime boots");
    let factory = per_provider_http;
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    match agent.run("chain-1", "hello") {
        RunResult::Answered { text, .. } => text,
        RunResult::Failed(reason) => panic!("turn failed: {reason}"),
    }
}

#[test]
fn the_configured_chain_decides_which_provider_is_tried_first() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-chain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let beta_first = config_with_chain(
        &dir,
        "providers:\n  - provider: beta\n  - provider: alpha\n",
    );
    assert_eq!(
        answer(&beta_first, &ext_dir),
        "from-beta",
        "the chain's first entry answers"
    );

    let alpha_first = config_with_chain(
        &dir,
        "providers:\n  - provider: alpha\n  - provider: beta\n",
    );
    assert_eq!(
        answer(&alpha_first, &ext_dir),
        "from-alpha",
        "reversing the chain reverses it"
    );
}

#[test]
fn an_unknown_name_in_the_chain_does_not_break_the_agent() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-chain-unknown-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let config = config_with_chain(
        &dir,
        "providers:\n  - provider: ollama\n  - provider: beta\n  - provider: alpha\n",
    );
    assert_eq!(answer(&config, &ext_dir), "from-beta");
}

#[test]
fn a_provider_the_chain_omits_is_still_reachable() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-chain-omit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let config = config_with_chain(&dir, "providers:\n  - provider: beta\n");
    assert_eq!(answer(&config, &ext_dir), "from-beta");

    let config = config_with_chain(&dir, "");
    assert_eq!(answer(&config, &ext_dir), "from-alpha");
}
