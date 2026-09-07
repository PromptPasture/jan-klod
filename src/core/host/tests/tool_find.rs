//! `tool-find` across the Component-Model boundary.
//!
//! Instantiates the `tool-find` guest against a real path-jailed workspace and
//! drives a glob over a nested tree: matches are found recursively, a pruned build
//! directory stays out unless it is named, and the jail's default-deny still holds
//! (no workspace → no walk). Offline.
//!
//! Skips (passes as a no-op) when the guest is not staged in `ext/`.

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

mod common;

fn find_component(engine: &Engine) -> Option<Component> {
    let path = common::repo_root().join("ext").join("tool-find.wasm");
    if !common::guests_staged(&["tool-find.wasm"]) {
        return None;
    }
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

#[test]
fn glob_walks_the_workspace_and_respects_the_jail() {
    let engine = Engine::default();
    let Some(component) = find_component(&engine) else {
        return;
    };

    let workspace_dir = std::env::temp_dir().join(format!("jk-toolfind-{}", std::process::id()));
    std::fs::create_dir_all(workspace_dir.join("src").join("util")).unwrap();
    std::fs::create_dir_all(workspace_dir.join("target").join("debug")).unwrap();
    std::fs::write(workspace_dir.join("README.md"), "docs").unwrap();
    std::fs::write(workspace_dir.join("src").join("main.rs"), "fn main() {}").unwrap();
    std::fs::write(
        workspace_dir.join("src").join("util").join("helper.rs"),
        "// h",
    )
    .unwrap();
    std::fs::write(
        workspace_dir.join("target").join("debug").join("build.rs"),
        "// b",
    )
    .unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.find",
        &component,
        Some(workspace),
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");

    // A slash-less pattern searches recursively; `target/` is pruned by default.
    let found = tool.invoke(r#"{"pattern":"*.rs"}"#).expect("glob succeeds");
    let paths: Vec<&str> = found.lines().collect();
    assert_eq!(
        paths,
        vec!["src/main.rs", "src/util/helper.rs"],
        "recursive + pruned: {found}"
    );

    // Naming the pruned directory opts back into it.
    let named = tool
        .invoke(r#"{"pattern":"target/**/*.rs"}"#)
        .expect("glob succeeds");
    assert_eq!(
        named.lines().collect::<Vec<_>>(),
        vec!["target/debug/build.rs"]
    );

    // `path` scopes the walk; the pattern is relative to it, results are not.
    let scoped = tool
        .invoke(r#"{"pattern":"*.rs","path":"src/util"}"#)
        .expect("glob succeeds");
    assert_eq!(
        scoped.lines().collect::<Vec<_>>(),
        vec!["src/util/helper.rs"]
    );

    // No match is an explicit statement, not an empty result.
    let empty = tool
        .invoke(r#"{"pattern":"**/*.zig"}"#)
        .expect("glob succeeds");
    assert!(
        empty.contains("no files match"),
        "empty result must say so: {empty}"
    );

    // The walk cannot leave the jail, even when asked directly.
    let escape = tool.invoke(r#"{"pattern":"*","path":".."}"#);
    assert!(
        escape.is_err(),
        "a root outside the workspace must be denied: {escape:?}"
    );

    std::fs::remove_dir_all(&workspace_dir).ok();
}

#[test]
fn tool_find_is_default_deny_without_a_workspace() {
    let engine = Engine::default();
    let Some(component) = find_component(&engine) else {
        return;
    };

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.find",
        &component,
        None,
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");
    let out = tool.invoke(r#"{"pattern":"**/*"}"#);
    assert!(
        out.is_err(),
        "with no workspace, host-fs must deny: {out:?}"
    );
}
