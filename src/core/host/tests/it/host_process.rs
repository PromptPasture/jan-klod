//! `host-process` across the Component-Model boundary.
//!
//! Instantiates the `tool-proc-probe` guest and drives its
//! `invoke({command, args})` through a bounded runner: a command runs and its
//! stdout comes back, and with execution disabled every call is denied
//! (default-deny). Offline.
//!
//! Skips (passes as a no-op) when the guest is not staged in `ext/`.

use std::time::Duration;

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::tool_host::ToolExtension;
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

    // Disabled runner -> exec denied.
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

/// A command run *through a guest* does not carry the host's credentials.
///
/// Unlike the `core::host_process` unit test, this covers the deployment path:
/// a sandboxed tool inherits an environment that necessarily holds
/// `OPENAI_API_KEY` (config expands `${...}`) and `JAN_KLOD_TOKEN`. A plain
/// `env` command would put both into tool output — which becomes a transcript
/// message sent to the model provider on the next turn.
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
    // And the environment is not simply empty — a command that gets no PATH
    // cannot run anything, which would make this pass for the wrong reason.
    assert!(out.contains("PATH="), "PATH is still provided: {out}");

    std::fs::remove_dir_all(&workspace_dir).ok();
}
