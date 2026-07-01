//! Phase 2 exit gate — the whole thin loop, end to end, offline.
//!
//! Boots the real `Runtime` from a `config.yaml` with **two provider instances**,
//! a `routing:` table, and **all five v1 interceptors enabled**, then runs a
//! multi-step query through `Runtime::build_agent` → `AgentSession::run`. The
//! first provider's `host-http` always fails, so the turn only completes if the
//! loop's **provider fallback** engages and the second provider answers — proving
//! the full pipeline (intent → shaping via the interceptors → completion with
//! fallback → grounded answer) runs through the sandboxed guests with no network.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`; build them
//! with `make ext`.

use std::cell::Cell;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jan_klod_core::conductor::{RunResult, ToolInvoker};
use jan_klod_core::http::{WireError, WireResponse};
use jan_klod_core::intercept::{Driver, ToolCall, UserPrompt};
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

/// An `host-http` backend that always fails the connection — the primary provider
/// uses this so the completion must fall back to the secondary.
fn failing_http() -> HttpFn {
    Box::new(|_m, _u, _h, _b, _t| Err(WireError::ConnectionFailed))
}

/// A canned chat-completions reply — the secondary provider answers with this.
fn canned_http(content: &'static str) -> HttpFn {
    Box::new(move |_m, _u, _h, _b, _t| {
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

const GUESTS: &[&str] = &[
    "provider-openai.wasm",
    "interceptor-intent-router.wasm",
    "interceptor-task-router.wasm",
    "interceptor-context.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

#[test]
fn phase2_exit_gate() {
    let ext_dir = repo_root().join("ext");
    for guest in GUESTS {
        if !ext_dir.join(guest).exists() {
            eprintln!("skipping: {guest} not staged — run `make ext`");
            return;
        }
    }

    let dir = std::env::temp_dir().join(format!("jk-phase2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "
extensions:
  store:
    memory:
      enabled: true
  provider:
    primary:
      enabled: true
      type: openai
      base-url: http://mock/v1
      model: primary-model
      api-key: test
    secondary:
      enabled: true
      type: openai
      base-url: http://mock/v1
      model: secondary-model
      api-key: test
  interceptor:
    intent-router:
      enabled: true
    task-router:
      enabled: true
    context:
      enabled: true
    tool-selector:
      enabled: true
    permission:
      enabled: true
routing:
  chat: primary/primary-model
",
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");

    // Providers boot in id order (primary, secondary); the first http_factory call
    // backs `primary` with a failing transport and the second backs `secondary`
    // with the canned success. So any completion must fall back to `secondary`.
    let call = Cell::new(0u32);
    let factory = || {
        let n = call.get();
        call.set(n + 1);
        if n == 0 {
            failing_http()
        } else {
            canned_http("grounded answer")
        }
    };
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    // A multi-step query: intent-router proceeds (agentic), the shaping
    // interceptors run (task-router resolves routing.chat -> primary-model, context
    // trims, tool-selector passes tools), then the completion falls back from the
    // failing primary to the answering secondary.
    let out = agent.run("gate-session", "Refactor the module and run the whole test suite");
    assert_eq!(
        out,
        RunResult::Answered { text: "grounded answer".into(), agentic: true },
        "the loop drives shaping + provider fallback to a grounded answer"
    );

    // A greeting short-circuits the agentic loop via the intent router (proving the
    // before-loop decision path), still answered inline by the fallback provider.
    let greeting = agent.run("gate-session", "hello");
    assert_eq!(
        greeting,
        RunResult::Answered { text: "grounded answer".into(), agentic: false },
        "a greeting is classified simple and short-circuits shaping"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// The integrated `ReAct` + permission path: a provider that first emits a tool call
/// then a final answer, gated by the real `interceptor-permission` guest whose
/// `ask` the driver approves, with a canned tool result fed back into the loop.
fn tool_then_answer_http() -> HttpFn {
    // First completion returns a (dangerous) tool call; the second returns text.
    let calls = Arc::new(AtomicU32::new(0));
    Box::new(move |_m, _u, _h, _b, _t| {
        let n = calls.fetch_add(1, Ordering::Relaxed);
        let body = if n == 0 {
            serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{
                            "id": "call-1",
                            "function": { "name": "bash", "arguments": "{}" }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
        } else {
            serde_json::json!({
                "choices": [{
                    "message": { "role": "assistant", "content": "all done" },
                    "finish_reason": "stop"
                }]
            })
        };
        Ok(WireResponse {
            status: 200,
            headers: vec![],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}

/// Driver that approves every `ask` and counts how often it was asked.
struct ApprovingDriver(Arc<AtomicU32>);
impl Driver for ApprovingDriver {
    fn ask(&mut self, _prompt: &UserPrompt) -> String {
        self.0.fetch_add(1, Ordering::Relaxed);
        "yes".to_string()
    }
}

/// A tool backend that returns a canned result and counts invocations.
struct CountingTools(Arc<AtomicU32>);
impl ToolInvoker for CountingTools {
    fn invoke(&mut self, _call: &ToolCall) -> Option<String> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Some("bash: ok".to_string())
    }
}

#[test]
fn phase2_gate_react_tool_call_with_permission() {
    let ext_dir = repo_root().join("ext");
    for guest in GUESTS {
        if !ext_dir.join(guest).exists() {
            eprintln!("skipping: {guest} not staged — run `make ext`");
            return;
        }
    }

    let dir = std::env::temp_dir().join(format!("jk-phase2-tools-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.yaml");
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
  interceptor:
    intent-router:
      enabled: true
    task-router:
      enabled: true
    context:
      enabled: true
    tool-selector:
      enabled: true
    permission:
      enabled: true
routing:
  chat: openai/mock-1
",
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = tool_then_answer_http;
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    let asked = Arc::new(AtomicU32::new(0));
    let invoked = Arc::new(AtomicU32::new(0));
    let mut driver = ApprovingDriver(Arc::clone(&asked));
    let mut tools = CountingTools(Arc::clone(&invoked));

    let out = agent.run_with(&mut driver, &mut tools, "gate-tools", "use bash to clean up, then report");

    assert_eq!(
        out,
        RunResult::Answered { text: "all done".into(), agentic: true },
        "the loop runs a ReAct cycle and returns the final answer"
    );
    assert_eq!(asked.load(Ordering::Relaxed), 1, "permission asked once for the dangerous tool");
    assert_eq!(invoked.load(Ordering::Relaxed), 1, "the approved tool ran once");
}
