//! Exit gates that prove the full loop end-to-end, offline.
//!
//! *Phase 2* — real `Runtime`, two provider instances, all five v1 interceptors.
//! The primary provider always fails so the turn only completes via fallback,
//! proving intent → shaping → completion-with-fallback → answer. A second test
//! drives ReAct + permission (tool call → driver approves → result fed back).
//!
//! *Phase 8* — same, plus `tool-fs` + a workspace. The provider emits a write
//! tool call that the fleet dispatches to the real guest, which writes through
//! `host-fs` — proving model → permission → fleet → host-fs → answer.
//!
//! Both skip (pass as a no-op) when guests are not staged in `ext/`.

use std::cell::Cell;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jan_klod_core::conductor::{RunResult, ToolInvoker};
use jan_klod_core::http::{WireError, WireResponse};
use jan_klod_core::intercept::{Driver, ToolCall, UserPrompt};
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

use crate::common;

// ── Phase 2 helpers ──────────────────────────────────────────────────────────

/// An `host-http` backend that always fails the connection — the primary provider
/// uses this so the completion must fall back to the secondary.
fn failing_http() -> HttpFn {
    Box::new(|_m, _u, _h, _b, _t| Err(WireError::ConnectionFailed))
}

const PHASE2_GUESTS: &[&str] = &[
    "provider-openai.wasm",
    "interceptor-intent-router.wasm",
    "interceptor-task-router.wasm",
    "interceptor-context.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

/// The integrated `ReAct` + permission path: a provider that first emits a tool call
/// then a final answer, gated by the real `interceptor-permission` guest whose
/// `ask` the driver approves, with a canned tool result fed back into the loop.
fn tool_then_answer_http() -> HttpFn {
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
struct CountingApprovingDriver(Arc<AtomicU32>);
impl Driver for CountingApprovingDriver {
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

// ── Phase 8 helpers ──────────────────────────────────────────────────────────

const PHASE8_GUESTS: &[&str] = &[
    "provider-openai.wasm",
    "interceptor-intent-router.wasm",
    "interceptor-task-router.wasm",
    "interceptor-context.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
    "tool-fs.wasm",
];

/// A provider that emits an `fs` `{"op":"write"}` tool call on the first completion, then a
/// final text answer on the second.
fn tool_calling_http() -> HttpFn {
    let calls = Arc::new(AtomicU32::new(0));
    Box::new(move |_m, _u, _h, _b, _t| {
        let n = calls.fetch_add(1, Ordering::Relaxed);
        let body = if n == 0 {
            let args = serde_json::json!({ "op": "write", "path": "out.txt", "contents": "hello from the tool" })
                .to_string();
            serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{ "id": "c1", "function": { "name": "fs", "arguments": args } }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
        } else {
            serde_json::json!({
                "choices": [{ "message": { "role": "assistant", "content": "wrote the file" }, "finish_reason": "stop" }]
            })
        };
        Ok(WireResponse {
            status: 200,
            headers: vec![("Content-Type".into(), "application/json".into())],
            body: serde_json::to_vec(&body).unwrap(),
        })
    })
}

/// A driver that approves every permission `ask` — a client clicking "allow".
struct ApprovingDriver;
impl Driver for ApprovingDriver {
    fn ask(&mut self, _prompt: &UserPrompt) -> String {
        "yes".to_string()
    }
}

// ── Phase 2 tests ────────────────────────────────────────────────────────────

#[test]
fn phase2_exit_gate() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(PHASE2_GUESTS) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-phase2-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "
extensions:
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

    // Providers boot in id order: the first http_factory call backs `primary`
    // with a failing transport, the second backs `secondary` with success.
    let call = Cell::new(0u32);
    let factory = || {
        let n = call.get();
        call.set(n + 1);
        if n == 0 {
            failing_http()
        } else {
            common::canned_http("grounded answer")
        }
    };
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    // A multi-step query: intent-router proceeds agentic, shaping interceptors
    // run, then completion falls back from the failing primary to secondary.
    let out = agent.run(
        "gate-session",
        "Refactor the module and run the whole test suite",
    );
    assert_eq!(
        out,
        RunResult::Answered {
            text: "grounded answer".into(),
            agentic: true
        },
        "the loop drives shaping + provider fallback to a grounded answer"
    );

    // A greeting short-circuits the agentic loop via the intent router (proving the
    // before-loop decision path), still answered inline by the fallback provider.
    let greeting = agent.run("gate-session", "hello");
    assert_eq!(
        greeting,
        RunResult::Answered {
            text: "grounded answer".into(),
            agentic: false
        },
        "a greeting is classified simple and short-circuits shaping"
    );

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn phase2_gate_react_tool_call_with_permission() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(PHASE2_GUESTS) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-phase2-tools-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.yaml");
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
    let mut driver = CountingApprovingDriver(Arc::clone(&asked));
    let mut tools = CountingTools(Arc::clone(&invoked));

    let out = agent.run_with(
        &mut driver,
        &mut tools,
        "gate-tools",
        "use bash to clean up, then report",
    );

    assert_eq!(
        out,
        RunResult::Answered {
            text: "all done".into(),
            agentic: true
        },
        "the loop runs a ReAct cycle and returns the final answer"
    );
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "permission asked once for the dangerous tool"
    );
    assert_eq!(
        invoked.load(Ordering::Relaxed),
        1,
        "the approved tool ran once"
    );
}

// ── Phase 8 tests ────────────────────────────────────────────────────────────

#[test]
fn phase8_exit_gate_tool_runs_through_the_loop() {
    let ext_dir = common::repo_root().join("ext");
    if !common::guests_staged(PHASE8_GUESTS) {
        return;
    }

    let dir = std::env::temp_dir().join(format!("jk-phase8-{}", std::process::id()));
    let _guard = common::TempDir(dir.clone());
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
extensions:
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
  tool:
    fs:
      enabled: true
routing:
  chat: openai/mock-1
workspace: {ws}
",
            ws = workspace.display()
        ),
    )
    .unwrap();

    let runtime = Runtime::boot(&config, &ext_dir).expect("runtime boots");
    let factory = tool_calling_http;
    let mut agent = runtime
        .build_agent(&factory)
        .expect("agent boots with tools");

    // The tool is advertised to the model.
    assert!(
        agent.tool_names().contains(&"fs".to_string()),
        "fleet: {:?}",
        agent.tool_names()
    );

    // `fs` with `{"op":"write"}` trips the permission gate (dangerous op); the driver
    // approves, so the tool runs — exercising the ask→approve→tool path with a real tool.
    let out = agent.run_driven(&mut ApprovingDriver, "gate-8", "please write out.txt");
    assert_eq!(
        out,
        RunResult::Answered {
            text: "wrote the file".into(),
            agentic: true
        },
        "the loop returns a grounded answer after the tool ran"
    );

    // The real tool wrote the file through host-fs, into the workspace.
    let written =
        std::fs::read_to_string(workspace.join("out.txt")).expect("the tool wrote the file");
    assert_eq!(written, "hello from the tool");
}
