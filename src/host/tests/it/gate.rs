//! Exit gates that prove the full loop end-to-end, offline.
//!
//! *Phase 2* — real `Runtime`, two provider instances, all five v1 interceptors.
//! The primary provider always fails so the turn only completes via fallback,
//! proving intent → shaping → completion-with-fallback → answer. A second test
//! drives `ReAct` + permission (tool call → driver approves → result fed back).
//!
//! *Phase 8* — same, plus `tool-fs` + a workspace. The provider emits a write
//! tool call that the fleet dispatches to the real guest, which writes through
//! `host-fs` — proving model → permission → fleet → host-fs → answer.
//!
//! *Phase 20* — two sessions over REST at the same time: one parked on a
//! confirmation while the other streams to completion, then the first
//! answered and finishing. Its control is the same pair in **one**
//! session, where they serialise.
//!
//! All skip when guests are not staged in `ext/`.

use std::cell::Cell;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use jan_klod_core::conductor::{RunResult, ToolInvocation, ToolInvoker};
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

/// Driver approving every `ask`, counting invocations.
struct CountingApprovingDriver(Arc<AtomicU32>);
impl Driver for CountingApprovingDriver {
    fn ask(&mut self, _prompt: &UserPrompt) -> String {
        self.0.fetch_add(1, Ordering::Relaxed);
        "yes".to_string()
    }
}

/// Tool backend returning canned result, counting calls.
struct CountingTools(Arc<AtomicU32>);
impl ToolInvoker for CountingTools {
    fn invoke(&mut self, _call: &ToolCall) -> Option<ToolInvocation> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Some(ToolInvocation {
            content: "bash: ok".to_string(),
            failed: false,
        })
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

/// Provider: fs write call (completion 0), then final answer (completion 1).
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

/// Driver approving every permission ask (user clicking "allow").
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

    // Call 0: primary fails. Call 1: secondary succeeds.
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

    // Multi-step: intent-router agentic, shaping runs, fallback to secondary.
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

    // Greeting short-circuits via intent router (before-loop path).
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

    // fs write trips permission gate; driver approves; tool runs (ask→approve).
    let out = agent.run_driven(&mut ApprovingDriver, "gate-8", "please write out.txt");
    assert_eq!(
        out,
        RunResult::Answered {
            text: "wrote the file".into(),
            agentic: true
        },
        "the loop returns a grounded answer after the tool ran"
    );

    // Tool wrote file through host-fs to workspace.
    let written =
        std::fs::read_to_string(workspace.join("out.txt")).expect("the tool wrote the file");
    assert_eq!(written, "hello from the tool");
}

// ── Phase 20 helpers ─────────────────────────────────────────────────────────

/// A runtime whose first completion per session calls a gated tool, so a
/// turn parks — unless the message says otherwise.
///
/// The *message* decides, not the session: both sessions get the same
/// provider, so what differs between them is only what they asked for.
fn phase20_fixture(
    tag: &str,
) -> Option<(
    common::TempDir,
    Arc<jan_klod_host::sessions::Agents>,
    jan_klod_host::serve::Surface,
)> {
    let needed = [
        "provider-openai.wasm",
        "interceptor-tool-selector.wasm",
        "interceptor-permission.wasm",
    ];
    if !common::guests_staged(&needed) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-phase20-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the temp dir");
    let guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
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
    tool-selector:
      enabled: true
    permission:
      enabled: true
",
            db = dir.join("jan-klod.db").display()
        ),
    )
    .expect("writes the config");

    let factory = std::sync::Arc::new(|| -> HttpFn {
        Box::new(move |_m, _u, _h, body, _t| {
            let asked = String::from_utf8_lossy(body.unwrap_or_default()).into_owned();
            // A turn that has already had its tool call answered says
            // "denied" or "yes" in its history; only the first completion
            // of a "use bash" message asks for the tool.
            let wants_tool = asked.contains("use bash") && !asked.contains("tool_call_id");
            let reply = if wants_tool {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"bash","arguments":"{}"}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"all done"},"finish_reason":"stop"}]})
            };
            Ok(WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&reply).expect("serialises"),
            })
        })
    });
    let runtime =
        Runtime::boot(&config, common::repo_root().join("ext")).expect("the runtime boots");
    let agents = jan_klod_host::sessions::Agents::new(runtime, factory);
    let surface =
        jan_klod_host::serve::Surface::bind("127.0.0.1:0").expect("binds an ephemeral port");
    Some((guard, agents, surface))
}

/// Open an SSE turn and keep the socket, so more can be read from it later.
fn phase20_open_stream(port: u16, session: &str, message: &str) -> TcpStream {
    let body = serde_json::json!({ "message": message }).to_string();
    let request = format!(
        "POST /session/{session}/message HTTP/1.1\r\nHost: localhost\r\n\
         Content-Type: application/json\r\nAccept: text/event-stream\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
    (&stream)
        .write_all(request.as_bytes())
        .expect("sends the turn");
    stream
}

/// Read from `stream` until `needle` appears, or the stream ends.
fn phase20_read_until(stream: &TcpStream, needle: &str) -> String {
    phase20_read_for(stream, needle, Duration::from_secs(30))
}

/// [`phase20_read_until`] with a deadline, for asserting something did
/// **not** arrive in a window.
fn phase20_read_for(stream: &TcpStream, needle: &str, within: Duration) -> String {
    stream
        .set_read_timeout(Some(within))
        .expect("a read timeout");
    let mut seen = String::new();
    let mut reader = BufReader::new(stream);
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) | Err(_) => return seen,
            Ok(_) => {
                seen.push_str(&line);
                if line.contains(needle) {
                    return seen;
                }
            }
        }
    }
}

/// `POST /session/{id}/answer`, returning the raw response.
fn phase20_answer(port: u16, session: &str, answer: &str) -> String {
    let body = serde_json::json!({ "answer": answer }).to_string();
    let request = format!(
        "POST /session/{session}/answer HTTP/1.1\r\nHost: localhost\r\n\
         Content-Type: application/json\r\nContent-Length: {}\r\n\
         Connection: close\r\n\r\n{body}",
        body.len()
    );
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connects");
    stream.write_all(request.as_bytes()).expect("sends");
    let mut response = String::new();
    let _ = std::io::Read::read_to_string(&mut stream, &mut response);
    response
}

// ── Phase 20 ─────────────────────────────────────────────────────────────────

/// Phase 20's exit gate:
///
/// > Two clients drive turns in two sessions **concurrently** over REST,
/// > with an `ask` answered in one while the other streams.
///
/// Parked is the only dependable "still running": against a canned
/// provider a computing turn finishes in milliseconds, so a test built on
/// one would be racing itself. A turn parked on the permission gate holds
/// still for as long as the answer timeout allows, which makes "while the
/// other was going" a fact rather than a hope — and it is what the gate
/// says anyway.
///
/// **Overlap, not ordering.** "B finished before A" is satisfied by luck.
/// "B finished while A was still parked" is the sentence.
#[test]
fn phase20_exit_gate_two_sessions_at_once() {
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "60");
    let Some((_guard, agents, surface)) = phase20_fixture("gate") else {
        return;
    };

    let (streamed, answered) = surface.serve_while(&agents, None, |port| {
        // A parks: the permission gate asks, and nothing answers yet.
        let parked = phase20_open_stream(port, "gate-a", "use bash to clean up");
        let asked = phase20_read_until(&parked, "event: prompt");
        assert!(asked.contains("event: prompt"), "A never parked: {asked}");

        // B runs to completion *while A is parked*. This is the gate.
        let streaming = phase20_open_stream(port, "gate-b", "just answer");
        let streamed = phase20_read_until(&streaming, "event: done");

        // Only now is A answered.
        let ack = phase20_answer(port, "gate-a", "yes");
        assert!(ack.contains("accepted"), "the answer was refused: {ack}");
        let answered = phase20_read_until(&parked, "event: done");
        (streamed, answered)
    });

    assert!(
        streamed.contains("event: done"),
        "the second session did not finish while the first was parked: {streamed}"
    );
    assert!(
        answered.contains("event: done"),
        "the parked session never finished after being answered: {answered}"
    );
}

/// The control, one changed string: **both messages in one session**.
///
/// Same fixture, same timings, same order — and they serialise, because
/// one session is one agent. Without this the test above passes on any
/// arrangement where B happens to be quick, including the one Phase 20
/// was filed to replace.
#[test]
fn phase20_one_session_still_serialises_the_same_two_turns() {
    std::env::set_var("JK_ANSWER_TIMEOUT_SECS", "20");
    let Some((_guard, agents, surface)) = phase20_fixture("control") else {
        return;
    };

    let finished_second = surface.serve_while(&agents, None, |port| {
        let parked = phase20_open_stream(port, "same", "use bash to clean up");
        assert!(
            phase20_read_until(&parked, "event: prompt").contains("event: prompt"),
            "the first turn never parked"
        );
        let second = phase20_open_stream(port, "same", "just answer");
        // Short: the point is that it does *not* arrive while the first
        // session's turn is parked.
        let early = phase20_read_for(&second, "event: done", Duration::from_millis(1500));
        let ack = phase20_answer(port, "same", "yes");
        assert!(ack.contains("accepted") || ack.contains("409"), "{ack}");
        let late = phase20_read_until(&second, "event: done");
        (early, late)
    });

    let (early, late) = finished_second;
    assert!(
        !early.contains("event: done"),
        "a second turn in the *same* session finished while the first was parked, \
         which one agent cannot do: {early}"
    );
    assert!(
        late.contains("event: done"),
        "and it should finish once the first turn is out of the way: {late}"
    );
}
