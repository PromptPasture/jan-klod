//! Phase 7 Slice 7a — `host-fs` across the Component-Model boundary.
//!
//! Instantiates the `tool-fs-probe` guest, which imports `host-fs`, and drives its
//! `invoke({path, contents})` (write→read) against a real path-jailed workspace:
//! a normal path round-trips, an escaping path is denied, and with no workspace
//! configured every op is denied (default-deny) — all offline.
//!
//! Skips (passes as a no-op) when the guest is not staged in `ext/`.

use std::path::PathBuf;

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

fn repo_root() -> PathBuf {
    [env!("CARGO_MANIFEST_DIR"), "..", "..", ".."].iter().collect()
}

fn probe_component(engine: &Engine) -> Option<Component> {
    let path = repo_root().join("ext").join("tool-fs-probe.wasm");
    if !path.exists() {
        eprintln!("skipping: tool-fs-probe.wasm not staged — run `make ext`");
        return None;
    }
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

#[test]
fn host_fs_round_trips_through_a_guest() {
    let engine = Engine::default();
    let Some(component) = probe_component(&engine) else { return };

    let workspace_dir = std::env::temp_dir().join(format!("jk-hostfs-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let mut tool =
        ToolExtension::instantiate(&engine, "tool.fs-probe", &component, Some(workspace), ProcessRunner::disabled())
            .expect("tool instantiates");

    // A normal write→read round-trips through host-fs.
    let out = tool
        .invoke(r#"{"path":"notes/todo.md","contents":"buy milk"}"#)
        .expect("invoke succeeds");
    assert_eq!(out, "buy milk");
    assert!(workspace_dir.join("notes/todo.md").exists(), "the file landed in the workspace");

    // An escaping path is denied by the path-jail (surfaced as a tool error).
    let escape = tool.invoke(r#"{"path":"../escape.txt","contents":"x"}"#);
    assert!(escape.is_err(), "an escaping path must be denied: {escape:?}");
    assert!(!workspace_dir.join("../escape.txt").exists());

    std::fs::remove_dir_all(&workspace_dir).ok();
}

#[test]
fn host_fs_is_default_deny_without_a_workspace() {
    let engine = Engine::default();
    let Some(component) = probe_component(&engine) else { return };

    // No workspace configured -> every host-fs op is denied.
    let mut tool =
        ToolExtension::instantiate(&engine, "tool.fs-probe", &component, None, ProcessRunner::disabled())
            .expect("tool instantiates");
    let out = tool.invoke(r#"{"path":"any.txt","contents":"x"}"#);
    assert!(out.is_err(), "with no workspace, host-fs must deny: {out:?}");
}
