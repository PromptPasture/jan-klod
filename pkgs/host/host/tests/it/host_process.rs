//! `host-process` through the `tool-proc-probe` guest: with execution disabled, every call is denied.

use std::time::Duration;

use jk_wasm::host_fs::Workspace;
use jk_wasm::host_process::ProcessRunner;
use jk_wasm::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

fn probe_component(engine: &Engine) -> Option<Component> {
    let path = common::repo_root().join("ext").join("tool-proc-probe.wasm");
    if !common::guests_staged(&["tool-proc-probe.wasm"]) {
        return None;
    }
    Some(Component::from_file(engine, &path).expect("component compiles"))
}

#[test]
fn host_process_runs_a_command_through_a_guest() {
    let engine = Engine::default();
    let Some(component) = probe_component(&engine) else {
        return;
    };

    let workspace_dir = std::env::temp_dir().join(format!("jk-hostproc-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");
    let runner = ProcessRunner::new(workspace, Duration::from_secs(5), 64 * 1024);

    let mut tool = ToolExtension::instantiate(&engine, "tool.proc-probe", &component, None, runner)
        .expect("tool instantiates");

    let out = tool
        .invoke(r#"{"command":"echo","args":["hello from exec"]}"#)
        .expect("invoke succeeds");
    assert_eq!(out.trim(), "hello from exec");

    std::fs::remove_dir_all(&workspace_dir).ok();
}

#[test]
fn host_process_is_default_deny_when_disabled() {
    let engine = Engine::default();
    let Some(component) = probe_component(&engine) else {
        return;
    };

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.proc-probe",
        &component,
        None,
        ProcessRunner::disabled(),
    )
    .expect("tool instantiates");
    let out = tool.invoke(r#"{"command":"echo","args":["x"]}"#);
    assert!(
        out.is_err(),
        "with execution disabled, host-process must deny: {out:?}"
    );
}

#[test]
fn a_guest_run_command_does_not_receive_the_hosts_credentials() {
    let engine = Engine::default();
    let Some(component) = probe_component(&engine) else {
        return;
    };

    std::env::set_var("OPENAI_API_KEY", "sk-guest-must-not-leak");
    std::env::set_var("JAN_KLOD_TOKEN", "bearer-guest-must-not-leak");

    let workspace_dir =
        std::env::temp_dir().join(format!("jk-hostproc-env-{}", std::process::id()));
    std::fs::create_dir_all(&workspace_dir).unwrap();
    let workspace = Workspace::open(&workspace_dir).expect("workspace opens");
    let runner = ProcessRunner::new(workspace, Duration::from_secs(5), 64 * 1024);

    let mut tool = ToolExtension::instantiate(&engine, "tool.proc-probe", &component, None, runner)
        .expect("tool instantiates");

    let out = tool
        .invoke(r#"{"command":"/bin/sh","args":["-c","env"]}"#)
        .unwrap_or_else(|err| err);
    assert!(
        !out.contains("must-not-leak"),
        "a credential reached the model through tool output:\n{out}"
    );
    assert!(out.contains("PATH="), "PATH is still provided: {out}");

    std::fs::remove_dir_all(&workspace_dir).ok();
}
