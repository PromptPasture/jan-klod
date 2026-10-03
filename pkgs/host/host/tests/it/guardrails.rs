//! `interceptor-guardrails` through the real loop.
//!
//! Unit tests over the guest's rule module prove matching; they cannot prove
//! that a match reaches the transcript or stops a tool. These two turns do,
//! and they are the acceptance of #177: a secret in model output never reaches
//! the transcript, and a denied tool argument blocks with the reason surfaced.
//!
//! Skips if guests are not staged in `ext/`; build with `make ext`.

use jan_klod_core::conductor::RunResult;
use jan_klod_core::Runtime;

use crate::common;

/// A fake API key, in the shape the rules match. Hard-coded rather than read
/// from anywhere: this file must never carry a real one, and a shape is all
/// the matcher sees.
const SECRET: &str = "sk-abcdefghijklmnop0123";

/// Boot a runtime from `config`, run one turn, and hand back the agent so the
/// caller can read what was persisted.
fn write_config(dir: &std::path::Path, body: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(dir).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(&config, body).unwrap();
    config
}

/// The model answers with a credential in it. The guardrail redacts at
/// `after-response` and again at `finalize`, and what the session records is
/// the redacted text.
///
/// Asserted against the **transcript**, not the stream. `finalize` edits the
/// authoritative message while the streamed preview has already gone out —
/// `wit/interceptor.wit` documents that divergence as intended, so watching
/// the stream would fail for the wrong reason.
#[test]
fn a_secret_in_the_model_output_never_reaches_the_transcript() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-guardrails.wasm"]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-guardrails-redact-{}", std::process::id()));
    let db = dir.join("jan-klod.db");
    let config = write_config(
        &dir,
        &format!(
            "
storage:
  path: {db}
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    guardrails:
      enabled: true
      redact:
        - pattern: \"sk-[A-Za-z0-9]{{16,}}\"
          with: \"[redacted]\"
",
            db = db.display()
        ),
    );

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    // The provider hands back an answer with the credential embedded in prose,
    // the way a model that just read a `.env` would.
    let factory = || common::canned_http("your key is sk-abcdefghijklmnop0123, keep it safe");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let out = agent.run("leak-1", "what is in the env file");
    let RunResult::Answered { text, .. } = out else {
        panic!("the turn completes: {out:?}");
    };
    assert!(
        !text.contains(SECRET),
        "the returned answer still carries the secret: {text}"
    );
    assert!(
        text.contains("[redacted]"),
        "the answer should show the replacement: {text}"
    );

    let transcript = agent.transcript("leak-1");
    let recorded = transcript
        .iter()
        .map(|m| m.content.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !recorded.contains(SECRET),
        "the secret reached the session log: {recorded}"
    );
    assert!(
        recorded.contains("[redacted]"),
        "the redacted answer is what was recorded: {recorded}"
    );
}

/// A tool call whose arguments match a deny rule never runs, and the reason
/// the operator wrote reaches the caller.
#[test]
fn a_denied_tool_argument_blocks_with_the_reason_surfaced() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(&[
        "provider-openai.wasm",
        "interceptor-guardrails.wasm",
        "tool-fs.wasm",
    ]) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-guardrails-deny-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    // A file the tool could read if nothing stopped it — so a passing test
    // means the guardrail refused, not that the read would have failed anyway.
    std::fs::write(dir.join("secrets.txt"), "contents\n").unwrap();
    let config = write_config(
        &dir,
        &format!(
            "
workspace: {work}
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  tool:
    fs:
      enabled: true
  interceptor:
    tool-selector:
      enabled: true
    guardrails:
      enabled: true
      deny-tool-arguments:
        - pattern: \"secrets\\\\.txt\"
          reason: \"the operator put this file out of bounds\"
",
            work = dir.display()
        ),
    );

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let factory = move || -> jan_klod_core::route::HttpFn {
        let calls = std::sync::Arc::clone(&calls);
        Box::new(move |_m, _u, _h, _b, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            // First the model reaches for the file; once the block comes back
            // as the tool result, it answers in words.
            let body = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"fs",
                     "arguments":"{\"op\":\"read\",\"path\":\"secrets.txt\"}"}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"I could not read it"},"finish_reason":"stop"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    };

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    let out = agent.run("deny-1", "read secrets.txt");
    assert!(
        matches!(out, RunResult::Answered { .. }),
        "a blocked tool ends the call, not the turn: {out:?}"
    );

    // The block travels back as the tool result, so the reason is in the
    // transcript where the model — and the user reading it — can see it.
    let recorded = agent
        .transcript("deny-1")
        .iter()
        .map(|m| m.content.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        recorded.contains("the operator put this file out of bounds"),
        "the rule's own reason should be surfaced: {recorded}"
    );
    assert!(
        !recorded.contains("contents"),
        "the file was read despite the guardrail: {recorded}"
    );
}
