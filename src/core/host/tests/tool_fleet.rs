//! Phase 8 Slice 8a — the loop's tool dispatch (`ToolFleet` as a `ToolInvoker`).
//!
//! Builds a fleet from the `tool-fs-probe` guest and dispatches a `ToolCall` by the
//! tool's advertised name (from `meta()`) through the `conductor::ToolInvoker` seam:
//! the matching extension runs and returns its result; an unknown tool is skipped.
//!
//! Skips (passes as a no-op) when the guest is not staged in `ext/`.

use std::path::PathBuf;

use jan_klod_core::conductor::ToolInvoker;
use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::intercept::ToolCall;
use jan_klod_core::tool_host::{ToolExtension, ToolFleet};
use wasmtime::component::Component;
use wasmtime::Engine;

fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall { id: "1".into(), name: name.into(), arguments: arguments.into() }
}

#[test]
fn fleet_dispatches_a_tool_call_by_name() {
    let engine = Engine::default();
    let path = repo_root().join("ext").join("tool-fs-probe.wasm");
    if !path.exists() {
        eprintln!("skipping: tool-fs-probe.wasm not staged — run `make ext`");
        return;
    }
    let component = Component::from_file(&engine, &path).expect("component compiles");

    let workspace_dir = std::env::temp_dir().join(format!("jk-fleet-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let mut probe =
        ToolExtension::instantiate(&engine, "tool.fs-probe", &component, Some(workspace), ProcessRunner::disabled())
            .expect("tool instantiates");
    // The probe advertises itself as `fs-probe`.
    assert_eq!(probe.meta().unwrap().name, "fs-probe");

    let mut fleet = ToolFleet::new(vec![probe]);
    assert_eq!(fleet.tool_names(), vec!["fs-probe".to_string()]);

    // A tool call named `fs-probe` reaches the extension and returns its result.
    let result = fleet.invoke(&call("fs-probe", r#"{"path":"a.txt","contents":"hi"}"#));
    assert_eq!(result.as_deref(), Some("hi"));

    // An unknown tool is skipped (None) — the loop tells the model "no tool".
    assert_eq!(fleet.invoke(&call("nonexistent", "{}")), None);

    std::fs::remove_dir_all(&workspace_dir).ok();
}
