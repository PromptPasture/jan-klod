//! Tool-edit: view → replace → reject cycle. Anchors apply partial edits,
//! stale anchors rejected, file untouched. Offline. Skips when guest not staged.

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

fn edit_component(engine: &Engine) -> Option<Component> {
    let path = common::repo_root().join("ext").join("tool-edit.wasm");
    if !common::guests_staged(&["tool-edit.wasm"]) {
        return None;
    }
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

/// Extract anchor from view's line (format: `anchor|lineno|text`).
fn anchor_at(view: &str, lineno: usize) -> String {
    let line = view.lines().nth(lineno - 1).expect("line is in the view");
    line.split('|')
        .next()
        .expect("anchor is the first field")
        .to_string()
}

#[test]
fn hash_anchored_edit_applies_and_a_stale_anchor_is_rejected() {
    let engine = Engine::default();
    let Some(component) = edit_component(&engine) else {
        return;
    };

    let workspace_dir = std::env::temp_dir().join(format!("jk-tooledit-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    std::fs::write(
        workspace_dir.join("main.rs"),
        "fn main() {\n    let x = 1;\n}\n",
    )
    .unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.edit",
        &component,
        Some(workspace),
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");

    // View gives one anchor per line.
    let view = tool
        .invoke(r#"{"op":"view","path":"main.rs"}"#)
        .expect("view succeeds");
    assert_eq!(
        view.lines().count(),
        3,
        "one anchored line per source line: {view}"
    );
    let body = anchor_at(&view, 2);

    // Replace touches only anchored line.
    let applied = tool
        .invoke(&format!(
            r#"{{"op":"replace","path":"main.rs","start":"{body}","contents":"    let x = 42;"}}"#
        ))
        .expect("replace succeeds");
    assert!(applied.contains("applied"), "edit confirmation: {applied}");
    assert_eq!(
        std::fs::read_to_string(workspace_dir.join("main.rs")).unwrap(),
        "fn main() {\n    let x = 42;\n}\n",
        "only the anchored line changed"
    );

    // Stale anchor refused (file untouched).
    let before = std::fs::read_to_string(workspace_dir.join("main.rs")).unwrap();
    let stale = tool
        .invoke(&format!(
            r#"{{"op":"replace","path":"main.rs","start":"{body}","contents":"    let x = 99;"}}"#
        ))
        .expect("a rejection is reported as a result, not a trap");
    assert!(
        stale.contains("REJECTED"),
        "stale anchor must be rejected: {stale}"
    );
    assert!(
        stale.contains("op=view"),
        "rejection must say how to recover: {stale}"
    );
    assert_eq!(
        std::fs::read_to_string(workspace_dir.join("main.rs")).unwrap(),
        before,
        "a rejected edit writes nothing"
    );

    // Insert re-anchored against current file.
    let view = tool
        .invoke(r#"{"op":"view","path":"main.rs"}"#)
        .expect("view succeeds");
    let opening = anchor_at(&view, 1);
    tool.invoke(&format!(
        r#"{{"op":"insert","path":"main.rs","after":"{opening}","contents":"    // hi"}}"#
    ))
    .expect("insert succeeds");
    assert_eq!(
        std::fs::read_to_string(workspace_dir.join("main.rs")).unwrap(),
        "fn main() {\n    // hi\n    let x = 42;\n}\n"
    );

    std::fs::remove_dir_all(&workspace_dir).ok();
}

#[test]
fn tool_edit_is_default_deny_without_a_workspace() {
    let engine = Engine::default();
    let Some(component) = edit_component(&engine) else {
        return;
    };

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.edit",
        &component,
        None,
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");
    let out = tool.invoke(r#"{"op":"view","path":"any.rs"}"#);
    assert!(out.is_err(), "no workspace denies operations: {out:?}");
}
