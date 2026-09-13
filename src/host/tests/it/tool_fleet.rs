//! The loop's tool dispatch (`ToolFleet` as a `ToolInvoker`).
//!
//! Builds a fleet from the `tool-fs` guest and dispatches a `ToolCall` by the
//! tool's advertised name through the `conductor::ToolInvoker` seam: the
//! matching extension runs; an unknown tool returns `None`. Skips when the
//! guest isn't staged in `ext/`.

use jan_klod_core::conductor::ToolInvoker;
use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::intercept::ToolCall;
use jan_klod_core::tool_host::{ToolExtension, ToolFleet};
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: "1".into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

#[test]
fn fleet_dispatches_a_tool_call_by_name() {
    let engine = Engine::default();
    let path = common::repo_root().join("ext").join("tool-fs.wasm");
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let component = Component::from_file(&engine, &path).expect("component compiles");

    let workspace_dir = std::env::temp_dir().join(format!("jk-fleet-{}", std::process::id()));
    let _guard = common::TempDir(workspace_dir.clone());
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let mut fs = ToolExtension::instantiate(
        &engine,
        "tool.fs",
        &component,
        Some(workspace),
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");
    // The tool advertises itself as `fs`.
    assert_eq!(fs.meta().unwrap().name, "fs");

    let mut fleet = ToolFleet::new(vec![fs]);
    assert_eq!(fleet.tool_names(), vec!["fs".to_string()]);

    // A tool call named `fs` reaches the extension and returns its result.
    let result = fleet.invoke(&call(
        "fs",
        r#"{"op":"write","path":"a.txt","contents":"hi"}"#,
    ));
    let result = result.expect("fs dispatched");
    assert!(result.content.contains("wrote a.txt"));
    // #162: a clearly-successful call must say so through the flag, not just
    // through prose a human would read.
    assert!(!result.failed, "a successful write is not `failed`");

    // An unknown tool is skipped (None) — the loop tells the model "no tool".
    assert_eq!(fleet.invoke(&call("nonexistent", "{}")), None);
}

fn load_tool(engine: &Engine, name: &str, workspace: Workspace) -> Option<ToolExtension> {
    let path = common::repo_root().join("ext").join(format!("{name}.wasm"));
    if !common::guests_staged(&[&format!("{name}.wasm")]) {
        return None;
    }
    let component = Component::from_file(engine, &path).expect("component compiles");
    Some(
        ToolExtension::instantiate(
            engine,
            name,
            &component,
            Some(workspace),
            ProcessRunner::disabled(),
        )
        .expect("tool instantiates"),
    )
}

#[test]
fn fs_tool_writes_reads_and_greps_through_the_fleet() {
    let engine = Engine::default();
    let workspace_dir = std::env::temp_dir().join(format!("jk-fstools-{}", std::process::id()));
    let _guard = common::TempDir(workspace_dir.clone());
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let Some(fs) = load_tool(&engine, "tool-fs", workspace) else {
        return;
    };
    let mut fleet = ToolFleet::new(vec![fs]);
    assert_eq!(fleet.tool_names(), vec!["fs".to_string()]);

    // The model emits one `fs` tool call per operation, dispatched by `op`.
    let written = fleet.invoke(&call(
        "fs",
        r#"{"op":"write","path":"src/main.rs","contents":"fn main(){}\nlet x=1;"}"#,
    ));
    assert!(written.unwrap().content.contains("wrote src/main.rs"));
    let read = fleet.invoke(&call("fs", r#"{"op":"read","path":"src/main.rs"}"#));
    assert_eq!(
        read.map(|i| i.content).as_deref(),
        Some("fn main(){}\nlet x=1;")
    );
    let grep_hits = fleet.invoke(&call(
        "fs",
        r#"{"op":"grep","pattern":"fn","path":"src/main.rs"}"#,
    ));
    assert_eq!(
        grep_hits.map(|i| i.content).as_deref(),
        Some("1:fn main(){}")
    );

    // A directory path (or none at all) greps the whole tree: hits carry their
    // path, so "where is this symbol?" is one call rather than find-then-read.
    fleet
        .invoke(&call(
            "fs",
            r#"{"op":"write","path":"src/util/helper.rs","contents":"fn help(){}"}"#,
        ))
        .expect("write succeeds");
    fleet
        .invoke(&call(
            "fs",
            r#"{"op":"write","path":"notes.md","contents":"fn in prose"}"#,
        ))
        .expect("write succeeds");

    let tree_hits = fleet.invoke(&call("fs", r#"{"op":"grep","pattern":"fn "}"#));
    assert_eq!(
        tree_hits.map(|i| i.content).as_deref(),
        Some("notes.md:1:fn in prose\nsrc/main.rs:1:fn main(){}\nsrc/util/helper.rs:1:fn help(){}")
    );

    // `glob` narrows the tree search to the files worth reading.
    let scoped = fleet.invoke(&call(
        "fs",
        r#"{"op":"grep","pattern":"fn ","glob":"**/*.rs"}"#,
    ));
    assert_eq!(
        scoped.map(|i| i.content).as_deref(),
        Some("src/main.rs:1:fn main(){}\nsrc/util/helper.rs:1:fn help(){}")
    );

    // `path` scopes it to a subtree — the .md file above is out of range.
    let subtree = fleet.invoke(&call("fs", r#"{"op":"grep","pattern":"fn ","path":"src"}"#));
    assert_eq!(
        subtree.map(|i| i.content).as_deref(),
        Some("src/main.rs:1:fn main(){}\nsrc/util/helper.rs:1:fn help(){}")
    );

    // No match is an explicit statement, not an empty string.
    let empty = fleet.invoke(&call("fs", r#"{"op":"grep","pattern":"zzz"}"#));
    assert_eq!(
        empty.map(|i| i.content).as_deref(),
        Some("no matches for zzz")
    );
}

#[test]
fn shell_tool_runs_a_command_through_the_fleet() {
    use std::time::Duration;

    let engine = Engine::default();
    let path = common::repo_root().join("ext").join("tool-shell.wasm");
    if !common::guests_staged(&["tool-shell.wasm"]) {
        return;
    }
    let component = Component::from_file(&engine, &path).expect("component compiles");

    let workspace_dir = std::env::temp_dir().join(format!("jk-shelltool-{}", std::process::id()));
    let _guard = common::TempDir(workspace_dir.clone());
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");
    let runner = ProcessRunner::new(workspace, Duration::from_secs(5), 64 * 1024);

    let shell = ToolExtension::instantiate(&engine, "tool.shell", &component, None, runner)
        .expect("tool instantiates");
    let mut fleet = ToolFleet::new(vec![shell]);
    assert_eq!(fleet.tool_names(), vec!["shell".to_string()]);

    let out = fleet
        .invoke(&call(
            "shell",
            r#"{"command":"echo","args":["from the shell tool"]}"#,
        ))
        .expect("shell dispatched");
    assert!(!out.failed, "a successful shell run is not `failed`");
    let out = out.content;
    let json: serde_json::Value = serde_json::from_str(&out).expect("shell returns JSON");
    assert_eq!(json["code"], 0);
    assert!(
        json["stdout"]
            .as_str()
            .unwrap()
            .contains("from the shell tool"),
        "stdout: {out}"
    );
}

/// A tree-wide grep does not sweep credentials into the transcript.
///
/// `fs:grep` is allowlisted to run without asking (a gate on every search gets
/// switched off) — but tool results become transcript sent to the provider, so
/// a grep for `password` in a repo with a `.env` would leak it, unasked and
/// unlogged. The gate sees only the *pattern*, not which files it will match,
/// so `guest-fs`'s walk skips credential files itself.
#[test]
fn a_tree_grep_skips_credential_files() {
    let engine = Engine::default();
    let workspace_dir = std::env::temp_dir().join(format!("jk-fsecret-{}", std::process::id()));
    let _guard = common::TempDir(workspace_dir.clone());
    std::fs::create_dir_all(workspace_dir.join("src")).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    // A secret in the two conventional shapes, plus a source file that mentions
    // the same word so the search is not trivially empty.
    std::fs::write(
        workspace_dir.join(".env"),
        "API_TOKEN=hunter2-must-not-leak\n",
    )
    .unwrap();
    std::fs::write(
        workspace_dir.join("deploy.pem"),
        "-----BEGIN KEY-----\nmust-not-leak\n",
    )
    .unwrap();
    std::fs::write(
        workspace_dir.join("src/config.rs"),
        "// reads API_TOKEN from the environment\n",
    )
    .unwrap();

    let Some(fs) = load_tool(&engine, "tool-fs", workspace) else {
        return;
    };
    let mut fleet = ToolFleet::new(vec![fs]);

    let hits = fleet
        .invoke(&call("fs", r#"{"op":"grep","pattern":"API_TOKEN"}"#))
        .map(|i| i.content)
        .unwrap_or_default();
    assert!(
        !hits.contains("must-not-leak"),
        "a credential reached the model through a search:\n{hits}"
    );
    // Not vacuous: the same grep still finds the ordinary file, so the walk is
    // working rather than returning nothing.
    assert!(
        hits.contains("src/config.rs"),
        "and the search still works on source: {hits}"
    );

    let listed = fleet.invoke(&call("fs", r#"{"op":"grep","pattern":"BEGIN KEY"}"#));
    assert!(
        !listed
            .map(|i| i.content)
            .unwrap_or_default()
            .contains("deploy.pem"),
        "a private key is not surfaced by content either"
    );
}
