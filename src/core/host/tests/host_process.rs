//! Phase 7 Slice 7b — `host-process` across the Component-Model boundary.
//!
//! Instantiates the `tool-proc-probe` guest (imports `host-process`) and drives its
//! `invoke({command, args})` through a bounded runner: a command runs and its
//! stdout comes back, and with execution disabled every call is denied
//! (default-deny) — offline.
//!
//! Skips (passes as a no-op) when the guest is not staged in `ext/`.

use std::path::PathBuf;
use std::time::Duration;

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

fn probe_component(engine: &Engine) -> Option<Component> {
    let path = repo_root().join("ext").join("tool-proc-probe.wasm");
    if !path.exists() {
        eprintln!("skipping: tool-proc-probe.wasm not staged — run `make ext`");
        return None;
    }
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

#[test]
fn host_process_runs_a_command_through_a_guest() {
    let engine = Engine::default();
    let Some(component) = probe_component(&engine) else { return };

    let workspace_dir = std::env::temp_dir().join(format!("jk-hostproc-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");
    let runner = ProcessRunner::new(workspace, Duration::from_secs(5), 64 * 1024);

    let mut tool = ToolExtension::instantiate(&engine, "tool.proc-probe", &component, None, runner)
        .expect("tool instantiates");

    let out = tool
        .invoke(r#"{"command":"echo","args":["hello from exec"]}"#)
        .expect("invoke succeeds");
    assert_eq!(out.trim(), "hello from exec");

    std::fs::remove_dir_all(&workspace_dir).ok();
}

#[test]
fn host_process_is_default_deny_when_disabled() {
    let engine = Engine::default();
    let Some(component) = probe_component(&engine) else { return };

    // Disabled runner -> exec denied.
    let mut tool =
        ToolExtension::instantiate(&engine, "tool.proc-probe", &component, None, ProcessRunner::disabled())
            .expect("tool instantiates");
    let out = tool.invoke(r#"{"command":"echo","args":["x"]}"#);
    assert!(out.is_err(), "with execution disabled, host-process must deny: {out:?}");
}
