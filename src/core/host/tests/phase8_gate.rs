//! Phase 8 exit gate — a real tool runs through the whole loop, offline.
//!
//! Boots the `Runtime` from a config with all v1 interceptors + the `tool-fs-write`
//! tool + a workspace, then runs a turn whose (canned) provider emits an `fs-write`
//! tool call. The call passes the `tool-call` permission gate (`fs-write` is not
//! dangerous), the fleet dispatches it to the real `tool-fs-write` extension which
//! writes the file **through `host-fs`**, the result feeds back, and the loop returns
//! a grounded answer. Proves the model → permission → fleet → host-fs → answer path.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jan_klod_core::conductor::RunResult;
use jan_klod_core::http::WireResponse;
use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::route::HttpFn;
use jan_klod_core::Runtime;

/// A driver that approves every permission `ask` — a client clicking "allow".
struct ApprovingDriver;
impl Driver for ApprovingDriver {
    fn ask(&mut self, _prompt: &UserPrompt) -> String {
        "yes".to_string()
    }
}

fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

const GUESTS: &[&str] = &[
    "provider-openai.wasm",
    "interceptor-intent-router.wasm",
    "interceptor-task-router.wasm",
    "interceptor-context.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
    "tool-fs-write.wasm",
];

/// A provider that emits an `fs-write` tool call on the first completion, then a
/// final text answer on the second.
fn tool_calling_http() -> HttpFn {
    let calls = Arc::new(AtomicU32::new(0));
    Box::new(move |_m, _u, _h, _b, _t| {
        let n = calls.fetch_add(1, Ordering::Relaxed);
        let body = if n == 0 {
            let args =
                serde_json::json!({ "path": "out.txt", "contents": "hello from the tool" }).to_string();
            serde_json::json!({
                "choices": [{
                    "message": {
                        "role": "assistant",
                        "tool_calls": [{ "id": "c1", "function": { "name": "fs-write", "arguments": args } }]
                    },
                    "finish_reason": "tool_calls"
                }]
            })
        } else {
            serde_json::json!({
                "choices": [{ "message": { "role": "assistant", "content": "wrote the file" }, "finish_reason": "stop" }]
            })
        };
        Ok(WireResponse { status: 200, headers: vec![], body: serde_json::to_vec(&body).unwrap() })
    })
}

#[test]
fn phase8_exit_gate_tool_runs_through_the_loop() {
    let ext_dir = repo_root().join("ext");
    for guest in GUESTS {
        if !ext_dir.join(guest).exists() {
            eprintln!("skipping: {guest} not staged — run `make ext`");
            return;
        }
    }

    let dir = std::env::temp_dir().join(format!("jk-phase8-{}", std::process::id()));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
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
  tool:
    fs-write:
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
    let mut agent = runtime.build_agent(&factory).expect("agent boots with tools");

    // The tool is advertised to the model.
    assert!(agent.tool_names().contains(&"fs-write".to_string()), "fleet: {:?}", agent.tool_names());

    // `fs-write` trips the permission gate (contains "write"); the driver approves,
    // so the tool runs — exercising the ask→approve→tool path with a real tool.
    let out = agent.run_driven(&mut ApprovingDriver, "gate-8", "please write out.txt");
    assert_eq!(
        out,
        RunResult::Answered { text: "wrote the file".into(), agentic: true },
        "the loop returns a grounded answer after the tool ran"
    );

    // The real tool wrote the file through host-fs, into the workspace.
    let written = std::fs::read_to_string(workspace.join("out.txt")).expect("the tool wrote the file");
    assert_eq!(written, "hello from the tool");

    std::fs::remove_dir_all(&dir).ok();
}
