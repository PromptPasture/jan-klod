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

fn load_tool(engine: &Engine, name: &str, workspace: Workspace) -> Option<ToolExtension> {
    let path = repo_root().join("ext").join(format!("{name}.wasm"));
    if !path.exists() {
        eprintln!("skipping: {name}.wasm not staged — run `make ext`");
        return None;
    }
    let component = Component::from_file(engine, &path).expect("component compiles");
    Some(
        ToolExtension::instantiate(engine, name, &component, Some(workspace), ProcessRunner::disabled())
            .expect("tool instantiates"),
    )
}

#[test]
fn fs_write_then_fs_read_through_the_fleet() {
    let engine = Engine::default();
    let workspace_dir = std::env::temp_dir().join(format!("jk-fstools-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");

    let (Some(writer), Some(reader), Some(grepper)) = (
        load_tool(&engine, "tool-fs-write", workspace.clone()),
        load_tool(&engine, "tool-fs-read", workspace.clone()),
        load_tool(&engine, "tool-fs-grep", workspace),
    ) else {
        return;
    };
    let mut fleet = ToolFleet::new(vec![writer, reader, grepper]);
    for name in ["fs-write", "fs-read", "fs-grep"] {
        assert!(fleet.tool_names().contains(&name.to_string()), "fleet has {name}");
    }

    // The model would emit fs-write then fs-read / fs-grep; drive them through the fleet.
    let written =
        fleet.invoke(&call("fs-write", r#"{"path":"src/main.rs","contents":"fn main(){}\nlet x=1;"}"#));
    assert!(written.unwrap().contains("wrote src/main.rs"));
    let read = fleet.invoke(&call("fs-read", r#"{"path":"src/main.rs"}"#));
    assert_eq!(read.as_deref(), Some("fn main(){}\nlet x=1;"));
    let grepped = fleet.invoke(&call("fs-grep", r#"{"pattern":"fn","path":"src/main.rs"}"#));
    assert_eq!(grepped.as_deref(), Some("1:fn main(){}"));

    std::fs::remove_dir_all(&workspace_dir).ok();
}

#[test]
fn shell_tool_runs_a_command_through_the_fleet() {
    use std::time::Duration;

    let engine = Engine::default();
    let path = repo_root().join("ext").join("tool-shell.wasm");
    if !path.exists() {
        eprintln!("skipping: tool-shell.wasm not staged — run `make ext`");
        return;
    }
    let component = Component::from_file(&engine, &path).expect("component compiles");

    let workspace_dir = std::env::temp_dir().join(format!("jk-shelltool-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");
    let runner = ProcessRunner::new(workspace, Duration::from_secs(5), 64 * 1024);

    let shell = ToolExtension::instantiate(&engine, "tool.shell", &component, None, runner)
        .expect("tool instantiates");
    let mut fleet = ToolFleet::new(vec![shell]);
    assert_eq!(fleet.tool_names(), vec!["shell".to_string()]);

    let out = fleet
        .invoke(&call("shell", r#"{"command":"echo","args":["from the shell tool"]}"#))
        .expect("shell dispatched");
    let json: serde_json::Value = serde_json::from_str(&out).expect("shell returns JSON");
    assert_eq!(json["code"], 0);
    assert!(json["stdout"].as_str().unwrap().contains("from the shell tool"), "stdout: {out}");

    std::fs::remove_dir_all(&workspace_dir).ok();
}
