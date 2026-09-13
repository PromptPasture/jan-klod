//! Interceptors reach a real model.
//!
//! `interceptor-intent-router` uses the model tier to decide simple-vs-agentic
//! when heuristics can't. `build_agent` used to pass a closure hardcoded to
//! `"agentic"`, so the classifier never actually ran in any shipped config.
//!
//! These drive `build_agent` against a canned provider and assert the
//! classifier is consulted, its verdict acted on, and a provider failure
//! degrades to the conservative label rather than failing the turn.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jan_klod_core::conductor::RunResult;
use jan_klod_core::http::WireResponse;
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

use crate::common;

const GUESTS: [&str; 2] = ["provider-openai.wasm", "interceptor-intent-router.wasm"];

/// A provider whose answer depends on which instance called it, so the test can
/// tell a classification call from the turn's own completion: the classifier is
/// pointed at `judge` (a distinct `base-url`) and everything else at `main`.
fn split_http(classifications: &Arc<AtomicU32>, verdict: &'static str) -> HttpFn {
    let classifications = Arc::clone(classifications);
    Box::new(move |_m, url: &str, _h, _b, _t| {
        let content = if url.contains("judge") {
            classifications.fetch_add(1, Ordering::Relaxed);
            verdict
        } else {
            "the full answer"
        };
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

fn write_config(dir: &std::path::Path, classifier: &str) -> std::path::PathBuf {
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
extensions:
  provider:
    main:
      enabled: true
      type: openai
      base-url: http://main/v1
      model: mock-1
      api-key: test
    judge:
      enabled: true
      type: openai
      base-url: http://judge/v1
      model: mock-small
      api-key: test
  interceptor:
    intent-router:
      enabled: true
providers:
  - provider: main
  - provider: judge
{classifier}
"
        ),
    )
    .unwrap();
    config
}

#[test]
fn the_classifier_is_consulted_and_its_verdict_is_acted_on() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-classifier-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    // `classifier: judge` points the model tier at the cheap instance — the
    // reason the key exists, since classification is a two-token question.
    let config = write_config(&dir, "classifier: judge\n");
    let calls = Arc::new(AtomicU32::new(0));

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let counted = Arc::clone(&calls);
    let factory = move || split_http(&counted, "simple");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    // A prompt the heuristics cannot settle, so the model tier runs.
    let out = agent.run(
        "c-1",
        "Tell me a fact and then do three unrelated things please",
    );

    assert_eq!(
        calls.load(Ordering::Relaxed),
        1,
        "the classifier was actually consulted"
    );
    // `simple` short-circuits the agentic path: the answer comes back marked
    // non-agentic, which is the whole point of classifying.
    match out {
        RunResult::Answered { agentic, .. } => {
            assert!(!agentic, "a `simple` verdict skips the agentic path");
        }
        RunResult::Failed(reason) => panic!("turn failed: {reason}"),
    }
}

#[test]
fn a_classifier_failure_takes_the_conservative_path() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-classifier-fail-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = write_config(&dir, "classifier: judge\n");

    // The classifier errors; the turn must still run, on the agentic path.
    let http = || -> HttpFn {
        Box::new(move |_m, url: &str, _h, _b, _t| {
            if url.contains("judge") {
                return Ok(WireResponse {
                    status: 500,
                    headers: vec![],
                    body: b"nope".to_vec(),
                });
            }
            let body = serde_json::json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "the full answer" },
                    "finish_reason": "stop"
                }]
            });
            Ok(WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    };

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime.build_agent(&http).expect("agent boots");
    let out = agent.run(
        "c-2",
        "Tell me a fact and then do three unrelated things please",
    );

    match out {
        RunResult::Answered { agentic, text } => {
            assert!(
                agentic,
                "a failed classification falls back to the full loop"
            );
            assert_eq!(text, "the full answer");
        }
        RunResult::Failed(reason) => {
            panic!("a classifier failure must not fail the turn: {reason}")
        }
    }
}

#[test]
fn an_unknown_classifier_name_does_not_break_the_agent() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let ext_dir = common::repo_root().join("ext");
    let dir = std::env::temp_dir().join(format!("jk-classifier-unknown-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    // Names a provider nobody enabled: a warning and the conservative default,
    // not a boot failure — the same posture as the fallback chain's own list.
    let config = write_config(&dir, "classifier: nonexistent\n");
    let calls = Arc::new(AtomicU32::new(0));
    let counted = Arc::clone(&calls);
    let factory = move || split_http(&counted, "simple");

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let out = agent.run(
        "c-3",
        "Tell me a fact and then do three unrelated things please",
    );

    assert_eq!(
        calls.load(Ordering::Relaxed),
        0,
        "no classifier instance was opened"
    );
    assert!(
        matches!(out, RunResult::Answered { agentic: true, .. }),
        "conservative default"
    );
}
