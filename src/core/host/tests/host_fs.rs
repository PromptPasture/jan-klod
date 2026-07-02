//! Phase 7 Slice 7a — `host-fs` across the Component-Model boundary.
//!
//! Instantiates the `tool-fs` guest, which imports `host-fs`, and drives its
//! `invoke({op, path, ...})` (write then read) against a real path-jailed workspace:
//! a normal path round-trips, an escaping path is denied, and with no workspace
//! configured every op is denied (default-deny) — all offline.
//!
//! Skips (passes as a no-op) when the guest is not staged in `ext/`.

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

mod common;

fn fs_component(engine: &Engine) -> Option<Component> {
    let path = common::repo_root().join("ext").join("tool-fs.wasm");
    if !path.exists() {
        eprintln!("skipping: tool-fs.wasm not staged — run `make ext`");
        return None;
    }
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

#[test]
fn host_fs_round_trips_through_a_guest() {
    let engine = Engine::default();
    let Some(component) = fs_component(&engine) else { return };

    let workspace_dir = std::env::temp_dir().join(format!("jk-hostfs-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let mut tool =
        ToolExtension::instantiate(&engine, "tool.fs", &component, Some(workspace), ProcessRunner::disabled())
            .expect("tool instantiates");

    // A write then read round-trips through host-fs.
    let wrote = tool
        .invoke(r#"{"op":"write","path":"notes/todo.md","contents":"buy milk"}"#)
        .expect("write succeeds");
    assert!(wrote.contains("wrote"), "write confirmation: {wrote}");
    assert!(workspace_dir.join("notes/todo.md").exists(), "the file landed in the workspace");
    let out = tool
        .invoke(r#"{"op":"read","path":"notes/todo.md"}"#)
        .expect("read succeeds");
    assert_eq!(out, "buy milk");

    // An escaping path is denied by the path-jail (surfaced as a tool error).
    let escape = tool.invoke(r#"{"op":"write","path":"../escape.txt","contents":"x"}"#);
    assert!(escape.is_err(), "an escaping path must be denied: {escape:?}");
    assert!(!workspace_dir.join("../escape.txt").exists());

    std::fs::remove_dir_all(&workspace_dir).ok();
}

#[test]
fn host_fs_is_default_deny_without_a_workspace() {
    let engine = Engine::default();
    let Some(component) = fs_component(&engine) else { return };

    // No workspace configured -> every host-fs op is denied.
    let mut tool =
        ToolExtension::instantiate(&engine, "tool.fs", &component, None, ProcessRunner::disabled())
            .expect("tool instantiates");
    let out = tool.invoke(r#"{"op":"write","path":"any.txt","contents":"x"}"#);
    assert!(out.is_err(), "with no workspace, host-fs must deny: {out:?}");
}
