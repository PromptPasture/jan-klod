//! #59: a `tool-*` guest is compiled at boot but not instantiated until the
//! fleet is actually asked for something.
//!
//! This is the direct, mechanism-level proof the issue asked for: not an
//! indirect signal (RSS, timing) but a flag on the fleet itself, checked
//! before and after each trigger. `ToolFleet`/`ToolExtension` are untouched —
//! every other test in `tool_fleet.rs` still builds a fleet from an
//! already-live `ToolExtension` — `LazyToolFleet` is the new layer above them
//! that defers the instantiate step those tests take for granted.
//!
//! Skips (passes as a no-op) when `tool-fs.wasm` isn't staged in `ext/`.

use jan_klod_core::conductor::ToolInvoker;
use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::intercept::ToolCall;
use jan_klod_core::tool_host::LazyToolFleet;
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
    );

    // Compiled (by the caller, above) is not instantiated: pushing a pending
    // tool must not itself run the guest.
    assert!(
        !fleet.is_instantiated(),
        "a freshly-pushed pending tool must not be live yet"
    );

    // The first thing that asks for metadata is what triggers it — the
    // `select-tools` advertising path in `build_agent`.
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
    );
    assert!(!fleet.is_instantiated());

    // Metadata was never asked for; the first `invoke` is what triggers it —
    // the actual `tool-call` dispatch path, distinct from advertising.
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
