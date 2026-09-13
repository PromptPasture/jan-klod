//! `tool-git` across the Component-Model boundary.
//!
//! Drives the read-only ops end to end against a real repo in a path-jailed
//! workspace: `status`/`log`/`diff` report real state through `host-process`,
//! a mutating subcommand is refused before any process spawns, and with
//! execution disabled everything is denied. Offline.
//!
//! Skips (passes as a no-op) when the guest isn't staged in `ext/`, or `git`
//! isn't on PATH.

use std::time::Duration;

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

fn git_component(engine: &Engine) -> Option<Component> {
    let path = common::repo_root().join("ext").join("tool-git.wasm");
    if !common::guests_staged(&["tool-git.wasm"]) {
        return None;
    }
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

/// Run `git` in `dir` for test setup, returning false if git is unavailable.
fn git(dir: &std::path::Path, args: &[&str]) -> bool {
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .is_ok_and(|out| out.status.success())
}

#[test]
fn read_only_ops_report_the_repository_and_mutations_are_refused() {
    let engine = Engine::default();
    let Some(component) = git_component(&engine) else {
        return;
    };

    let workspace_dir = std::env::temp_dir().join(format!("jk-toolgit-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let _guard = common::TempDir(workspace_dir.clone());

    if !common::tool_available("git") {
        return;
    }
    assert!(
        git(&workspace_dir, &["init", "--quiet"]),
        "git init succeeds"
    );
    // A committer identity, set locally so the test never depends on (or touches)
    // the machine's global git config.
    git(
        &workspace_dir,
        &["config", "user.email", "test@example.com"],
    );
    git(&workspace_dir, &["config", "user.name", "Test"]);
    std::fs::write(workspace_dir.join("main.rs"), "fn main() {}\n").unwrap();
    git(&workspace_dir, &["add", "main.rs"]);
    assert!(
        git(&workspace_dir, &["commit", "--quiet", "-m", "add main"]),
        "commit succeeds"
    );
    // An uncommitted edit, so `status` and `diff` have something to report.
    std::fs::write(workspace_dir.join("main.rs"), "fn main() { todo!() }\n").unwrap();

    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");
    let runner = ProcessRunner::new(workspace.clone(), Duration::from_secs(20), 64 * 1024);
    let mut tool =
        ToolExtension::instantiate(&engine, "tool.git", &component, Some(workspace), runner)
            .expect("tool instantiates");

    let status = tool.invoke(r#"{"op":"status"}"#).expect("status succeeds");
    assert!(
        status.contains("main.rs"),
        "the modified file is reported: {status}"
    );

    let log = tool
        .invoke(r#"{"op":"log","count":5}"#)
        .expect("log succeeds");
    assert!(
        log.contains("add main"),
        "the commit subject is reported: {log}"
    );

    let diff = tool.invoke(r#"{"op":"diff"}"#).expect("diff succeeds");
    assert!(
        diff.contains("todo!()"),
        "the working-tree change is reported: {diff}"
    );

    // Nothing is staged, so the staged diff is empty — and says so rather than
    // returning a bare empty string the model would have to interpret.
    let staged = tool
        .invoke(r#"{"op":"diff","staged":true}"#)
        .expect("staged diff succeeds");
    assert_eq!(staged, "(no output)");

    let show = tool
        .invoke(r#"{"op":"show","rev":"HEAD"}"#)
        .expect("show succeeds");
    assert!(show.contains("add main"), "the commit is shown: {show}");

    // The write half of git is not expressible: refused by the guest, so no
    // process is spawned at all.
    for mutating in [
        r#"{"op":"commit"}"#,
        r#"{"op":"push"}"#,
        r#"{"op":"checkout","rev":"HEAD"}"#,
        r#"{"op":"reset"}"#,
    ] {
        let out = tool
            .invoke(mutating)
            .expect("a refusal is a result, not a trap");
        assert!(
            out.starts_with("REFUSED:"),
            "{mutating} must be refused: {out}"
        );
        assert!(out.contains("reads a repository only"), "{out}");
    }

    // An argument that would smuggle a flag past the op allowlist is refused too.
    let smuggled = tool
        .invoke(r#"{"op":"show","rev":"--upload-pack=touch /tmp/pwned"}"#)
        .expect("a refusal is a result");
    assert!(smuggled.starts_with("REFUSED:"), "{smuggled}");

    // The repository is exactly as it was: read-only means read-only.
    let after = tool
        .invoke(r#"{"op":"log","count":5}"#)
        .expect("log succeeds");
    assert_eq!(after.lines().count(), 1, "still one commit: {after}");
}

#[test]
fn tool_git_is_default_deny_without_execution() {
    let engine = Engine::default();
    let Some(component) = git_component(&engine) else {
        return;
    };

    // A workspace but no execution substrate: the tool loads and still cannot run.
    let workspace_dir =
        std::env::temp_dir().join(format!("jk-toolgit-deny-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let _guard = common::TempDir(workspace_dir.clone());
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.git",
        &component,
        Some(workspace),
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");

    let out = tool.invoke(r#"{"op":"status"}"#);
    assert!(
        out.is_err(),
        "with execution disabled, host-process must deny: {out:?}"
    );
}
