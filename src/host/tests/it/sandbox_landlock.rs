//! Linux Landlock confinement tests (same four cases as `sandbox_seatbelt.rs`).
//! Two backends, one standard; unconfined baseline catches test errors.
//!
//! Landlock is kernel-dependent (pre-5.13 or no CONFIG_SECURITY_LANDLOCK).
//! Check availability first to avoid ambiguous failures.
//!
//! Backend path is explicit (CARGO_BIN_EXE_jan-klod-gateway) because
//! `current_exe()` under `cargo test` is the test binary without `confine`.
#![cfg(target_os = "linux")]

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::sandbox::{SandboxMode, SandboxPolicy};
use jan_klod_core::sandbox_landlock::{available, LandlockBackend};
use jan_klod_core::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

/// Workspace and external sibling; auto-cleaned.
struct Dirs {
    root: PathBuf,
    workspace: PathBuf,
    outside: PathBuf,
}

impl Drop for Dirs {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn dirs(tag: &str) -> Dirs {
    let root = std::env::temp_dir().join(format!("jk-landlock-{tag}-{}", std::process::id()));
    let workspace = root.join("workspace");
    let outside = root.join("outside");
    std::fs::create_dir_all(&workspace).expect("creates the workspace");
    std::fs::create_dir_all(&outside).expect("creates the sibling");
    Dirs {
        root,
        workspace,
        outside,
    }
}

/// Fail with clear message if Landlock unavailable (not ambiguous denial).
fn require_landlock() {
    available().unwrap_or_else(|reason| {
        panic!("these tests assert Landlock's behaviour and it is unavailable: {reason}")
    });
}

fn policy(workspace: &Path) -> SandboxPolicy {
    SandboxPolicy {
        mode: SandboxMode::Os,
        writable: vec![workspace.to_path_buf()],
        network: false,
        require: false,
    }
}

fn runner(workspace: &Path) -> ProcessRunner {
    ProcessRunner::new(
        Workspace::open(workspace).expect("the workspace opens"),
        Duration::from_secs(10),
        64 * 1024,
    )
}

/// Runner confined by gateway (path explicit; see module docs).
fn confined(workspace: &Path) -> ProcessRunner {
    let backend = LandlockBackend::new(PathBuf::from(env!("CARGO_BIN_EXE_jan-klod-gateway")));
    runner(workspace).with_sandbox(std::sync::Arc::new(backend), policy(workspace))
}

/// Baseline control (unconfined).
fn unconfined(workspace: &Path) -> ProcessRunner {
    runner(workspace)
}

fn write_to(runner: &ProcessRunner, target: &Path) -> i32 {
    let args = vec!["-c".to_owned(), format!("echo x > {}", target.display())];
    runner
        .exec("/bin/sh", &args, None, None)
        .expect("the command runs")
        .code
}

#[test]
fn a_confined_command_cannot_write_outside_the_workspace() {
    require_landlock();
    let dirs = dirs("escape");
    let target = dirs.outside.join("leak");

    assert_eq!(write_to(&unconfined(&dirs.workspace), &target), 0);
    assert!(target.exists(), "control baseline");
    std::fs::remove_file(&target).expect("cleanup");

    assert_ne!(
        write_to(&confined(&dirs.workspace), &target),
        0,
        "a write outside the workspace is denied"
    );
    assert!(
        !target.exists(),
        "and nothing was written: {}",
        target.display()
    );
}

/// Catches broken sandbox (wrapper not found or ruleset empty).
#[test]
fn a_confined_command_can_still_write_inside_the_workspace() {
    require_landlock();
    let dirs = dirs("inside");
    let target = dirs.workspace.join("ok");
    assert_eq!(
        write_to(&confined(&dirs.workspace), &target),
        0,
        "grant works (wrapper found, ruleset correct)"
    );
    assert!(target.exists());
}

/// `2>/dev/null` works, which it did not before #212. The Linux twin of
/// `sandbox_seatbelt.rs::a_confined_command_can_redirect_to_dev_null`.
///
/// The shell opens the redirect before running the program, so a denied
/// `/dev/null` means the command never runs and the error names `/dev/null`
/// rather than anything the operator wrote.
#[test]
fn a_confined_command_can_redirect_to_dev_null() {
    require_landlock();
    let dirs = dirs("devnull");
    let args = vec!["-c".to_owned(), "echo hi 2>/dev/null".to_owned()];
    let out = confined(&dirs.workspace)
        .exec("/bin/sh", &args, None, None)
        .expect("the command runs");
    assert_eq!(
        out.code, 0,
        "a shell redirecting to /dev/null runs: {}",
        out.stderr
    );
    assert!(
        out.stdout.contains("hi"),
        "and the program itself ran: {out:?}"
    );
}

/// The other half, and the reason the grant above is not a hole: the rule
/// names the device, so the rest of `/dev` stays unwritable. Landlock scopes
/// a file rule to that file, and this is what notices if it is ever widened
/// to the directory.
#[test]
fn granting_dev_null_does_not_grant_the_device_directory() {
    require_landlock();
    let dirs = dirs("devdir");
    let args = vec!["-c".to_owned(), "echo x > /dev/jan-klod-probe".to_owned()];
    let out = confined(&dirs.workspace)
        .exec("/bin/sh", &args, None, None)
        .expect("the command runs");
    assert_ne!(
        out.code, 0,
        "a write elsewhere under /dev is still denied: {out:?}"
    );
    assert!(
        !Path::new("/dev/jan-klod-probe").exists(),
        "and nothing was created in /dev"
    );
}

/// Network denial via listener (closed port and denial look the same).
#[test]
fn a_confined_command_cannot_reach_the_network() {
    require_landlock();
    if !common::tool_available("curl") {
        return;
    }
    let dirs = dirs("network");
    let listener = TcpListener::bind("127.0.0.1:0").expect("binds a port");
    let port = listener.local_addr().expect("has an address").port();
    let (arrived, connections) = mpsc::channel();
    thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(mut stream) = stream {
                let mut discard = [0_u8; 64];
                let _ = stream.read(&mut discard);
            }
            if arrived.send(()).is_err() {
                return;
            }
        }
    });

    let url = format!("http://127.0.0.1:{port}/");
    let args = vec!["-s".to_owned(), "-m".to_owned(), "3".to_owned(), url];

    let _ = confined(&dirs.workspace).exec("curl", &args, None, None);
    assert!(
        connections.recv_timeout(Duration::from_secs(2)).is_err(),
        "the confined command reached a listener it should not have"
    );

    let _ = unconfined(&dirs.workspace).exec("curl", &args, None, None);
    assert!(
        connections.recv_timeout(Duration::from_secs(5)).is_ok(),
        "the control never arrived either, so the case above proved nothing about the \
         sandbox — check that curl works and the listener is accepting"
    );
}

/// The same denial, reached the way a component reaches it.
#[test]
fn a_guest_running_a_command_is_confined_too() {
    require_landlock();
    let engine = Engine::default();
    if !common::guests_staged(&["tool-proc-probe.wasm"]) {
        return;
    }
    let component = Component::from_file(
        &engine,
        common::repo_root().join("ext").join("tool-proc-probe.wasm"),
    )
    .expect("the component compiles");

    let dirs = dirs("guest");
    let target = dirs.outside.join("leak");
    let request = format!(
        r#"{{"command":"/bin/sh","args":["-c","echo x > {}"]}}"#,
        target.display()
    );

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.proc-probe",
        &component,
        None,
        unconfined(&dirs.workspace),
    )
    .expect("the tool instantiates");
    let _ = tool.invoke(&request).expect("the call is served");
    assert!(
        target.exists(),
        "the control wrote through the guest, so the denial below is the sandbox's"
    );
    std::fs::remove_file(&target).expect("removes it again");

    let mut tool = ToolExtension::instantiate(
        &engine,
        "tool.proc-probe",
        &component,
        None,
        confined(&dirs.workspace),
    )
    .expect("the tool instantiates");
    let _ = tool.invoke(&request).expect("the call is served");
    assert!(
        !target.exists(),
        "a command a guest asked for is confined too: {}",
        target.display()
    );
}
