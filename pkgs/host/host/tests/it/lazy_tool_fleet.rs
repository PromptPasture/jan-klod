//! #59: a `tool-*` guest is compiled at boot but not instantiated until first use. Checked via a flag on the fleet.
//! Skips when `tool-fs.wasm` is not staged in `ext/`.

use jk_protocol::intercept::ToolInvoker;
use jk_protocol::turn::ToolCall;
use jk_wasm::host_fs::Workspace;
use jk_wasm::host_process::ProcessRunner;
use jk_wasm::tool_host::LazyToolFleet;
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

/// A workspace opened in a fresh temp dir, cleaned up on drop.
fn workspace(tag: &str) -> (Workspace, common::TempDir) {
    let dir = std::env::temp_dir().join(format!("jk-lazyfleet-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let guard = common::TempDir(dir.clone());
    (Workspace::open(&dir).expect("workspace opens"), guard)
}

#[test]
fn a_pending_tool_is_not_instantiated_until_metadata_is_asked_for() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let engine = Engine::default();
    let path = common::repo_root().join("ext").join("tool-fs.wasm");
    let component = Component::from_file(&engine, &path).expect("component compiles");
    let (ws, _guard) = workspace("meta");

    let mut fleet = LazyToolFleet::new(engine);
    fleet.push(
        "tool.fs",
        component,
        Some(ws),
        ProcessRunner::disabled(),
        None,
        None,
        false,
    );

    // Pushing a pending tool must not run the guest.
    assert!(
        !fleet.is_instantiated(),
        "a freshly-pushed pending tool must not be live yet"
    );

    // Metadata request (the `select-tools` path in `build_agent`) triggers it.
    let names = fleet.tool_names().expect("the pending tool instantiates");
    assert_eq!(names, vec!["fs".to_string()]);
    assert!(
        fleet.is_instantiated(),
        "asking for metadata must instantiate the pending set"
    );
}

#[test]
fn a_pending_tool_is_not_instantiated_until_invoked() {
    if !common::guests_staged(&["tool-fs.wasm"]) {
        return;
    }
    let engine = Engine::default();
    let path = common::repo_root().join("ext").join("tool-fs.wasm");
    let component = Component::from_file(&engine, &path).expect("component compiles");
    let (ws, _guard) = workspace("invoke");

    let mut fleet = LazyToolFleet::new(engine);
    fleet.push(
        "tool.fs",
        component,
        Some(ws),
        ProcessRunner::disabled(),
        None,
        None,
        false,
    );
    assert!(!fleet.is_instantiated());

    // The `tool-call` dispatch path triggers it, distinct from advertising.
    let result = fleet.invoke(&call(
        "fs",
        r#"{"op":"write","path":"a.txt","contents":"hi"}"#,
    ));
    let result = result.expect("fs dispatched");
    assert!(result.content.contains("wrote a.txt"));
    assert!(!result.failed, "a successful write is not `failed`");
    assert!(
        fleet.is_instantiated(),
        "invoking a pending tool must instantiate it"
    );
}
