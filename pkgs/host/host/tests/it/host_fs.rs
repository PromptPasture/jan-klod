//! `host-fs` through the `tool-fs` guest: workspace escapes are denied, and with no workspace everything is denied.

use jk_wasm::host_fs::Workspace;
use jk_wasm::host_process::ProcessRunner;
use jk_wasm::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

fn fs_component(engine: &Engine) -> Option<Component> {
    let path = common::repo_root().join("ext").join("tool-fs.wasm");
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return None;
    }
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

#[test]
fn host_fs_round_trips_through_a_guest() {
    let engine = Engine::default();
    let Some(component) = fs_component(&engine) else {
        return;
    };

    let workspace_dir = std::env::temp_dir().join(format!("jk-hostfs-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.fs",
        &component,
        Some(workspace),
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");

    let wrote = tool
        .invoke(r#"{"op":"write","path":"notes/todo.md","contents":"buy milk"}"#)
        .expect("write succeeds");
    assert!(wrote.contains("wrote"), "write confirmed: {wrote}");
    assert!(
        workspace_dir.join("notes/todo.md").exists(),
        "file landed in workspace"
    );
    let out = tool
        .invoke(r#"{"op":"read","path":"notes/todo.md"}"#)
        .expect("read succeeds");
    assert_eq!(out, "buy milk");

    let escape = tool.invoke(r#"{"op":"write","path":"../escape.txt","contents":"x"}"#);
    assert!(
        escape.is_err(),
        "an escaping path must be denied: {escape:?}"
    );
    assert!(!workspace_dir.join("../escape.txt").exists());

    std::fs::remove_dir_all(&workspace_dir).ok();
}

#[test]
fn host_fs_is_default_deny_without_a_workspace() {
    let engine = Engine::default();
    let Some(component) = fs_component(&engine) else {
        return;
    };

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.fs",
        &component,
        None,
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");
    let out = tool.invoke(r#"{"op":"write","path":"any.txt","contents":"x"}"#);
    assert!(
        out.is_err(),
        "with no workspace, host-fs must deny: {out:?}"
    );
}
