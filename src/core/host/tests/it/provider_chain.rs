//! The `providers:` fallback chain actually orders the fallback.
//!
//! `build_agent` used to ignore the configured list and assemble providers in
//! alphabetical boot order, so editing `providers:` changed nothing. Two
//! providers answer with distinguishable text so the test can name which one
//! was tried first — e.g. listing `beta` first makes it answer even though
//! `alpha` sorts earlier.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use jan_klod_core::conductor::RunResult;
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

use crate::common;

/// Each provider instance has its own `base-url`, so one canned client can answer
/// as whichever provider is calling.
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

/// Two enabled providers; `chain` is the top-level `providers:` block.
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

    // `beta` first, against alphabetical order — the case the old behaviour failed.
    let beta_first = config_with_chain(
        &dir,
        "providers:\n  - provider: beta\n  - provider: alpha\n",
    );
    assert_eq!(
        answer(&beta_first, &ext_dir),
        "from-beta",
        "the chain's first entry answers"
    );

    // Reversing the list reverses the result: the order is genuinely read, not
    // coincidentally matching some other ordering.
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

    // `ollama` is listed but never enabled — a warning, not a boot failure;
    // enabled providers still run in the order given.
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

    // `alpha` is enabled but unlisted — it must still be reachable (not
    // silently dropped), just after the listed entries, so `beta` answers first.
    let config = config_with_chain(&dir, "providers:\n  - provider: beta\n");
    assert_eq!(answer(&config, &ext_dir), "from-beta");

    // With no chain at all, boot order stands and `alpha` (alphabetically first)
    // answers — the documented default when the key is absent.
    let config = config_with_chain(&dir, "");
    assert_eq!(answer(&config, &ext_dir), "from-alpha");
}
