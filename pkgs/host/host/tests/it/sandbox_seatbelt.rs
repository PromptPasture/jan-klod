//! Seatbelt confinement on macOS: escape writes denied, in-workspace writes allowed, network refused. Not run on Linux CI (#95).
//! The in-workspace write is the canary: Seatbelt matches resolved paths, and temp dirs sit under a symlink.
#![cfg(target_os = "macos")]

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jk_wasm::host_fs::Workspace;
use jk_wasm::host_process::ProcessRunner;
use jk_wasm::sandbox::{self, SandboxMode, SandboxPolicy};
use jk_wasm::tool_host::ToolExtension;
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

/// A workspace directory and a sibling that is outside it, removed on drop.
struct Sandboxed {
    root: PathBuf,
    workspace: PathBuf,
    outside: PathBuf,
}

impl Drop for Sandboxed {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn dirs(tag: &str) -> Sandboxed {
    let root = std::env::temp_dir().join(format!("jk-seatbelt-{tag}-{}", std::process::id()));
    let workspace = root.join("workspace");
    let outside = root.join("outside");
    std::fs::create_dir_all(&workspace).expect("creates the workspace");
    std::fs::create_dir_all(&outside).expect("creates the sibling");
    Sandboxed {
        root,
        workspace,
        outside,
    }
}

/// Policy for `workspace`: `os` mode, one directory writable, no network.
fn policy(workspace: &Path) -> SandboxPolicy {
    SandboxPolicy {
        mode: SandboxMode::Os,
        writable: vec![workspace.to_path_buf()],
        network: false,
        require: false,
    }
}

/// A runner over `workspace`, confined by the host backend.
fn confined(workspace: &Path) -> ProcessRunner {
    let backend = sandbox::host_backend().expect("macOS has a backend; see sandbox.rs's own test");
    ProcessRunner::new(
        Workspace::open(workspace).expect("the workspace opens"),
        Duration::from_secs(10),
        64 * 1024,
    )
    .with_sandbox(std::sync::Arc::from(backend), policy(workspace))
}

/// Unconfined control for each case, so a failure is attributable to the sandbox.
fn unconfined(workspace: &Path) -> ProcessRunner {
    ProcessRunner::new(
        Workspace::open(workspace).expect("the workspace opens"),
        Duration::from_secs(10),
        64 * 1024,
    )
}

/// `sh -c "echo x > <target>"`, as a guest would request.
fn write_to(runner: &ProcessRunner, target: &Path) -> i32 {
    let args = vec!["-c".to_owned(), format!("echo x > {}", target.display())];
    runner
        .exec("/bin/sh", &args, None, None)
        .expect("the command runs")
        .code
}

#[test]
fn a_confined_command_cannot_write_outside_the_workspace() {
    let dirs = dirs("escape");
    let target = dirs.outside.join("leak");

    assert_eq!(write_to(&unconfined(&dirs.workspace), &target), 0);
    assert!(target.exists(), "the control wrote the file");
    std::fs::remove_file(&target).expect("removes it again");

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

/// The canary for canonicalization bugs (see module docs).
#[test]
fn a_confined_command_can_still_write_inside_the_workspace() {
    let dirs = dirs("inside");
    let target = dirs.workspace.join("ok");
    assert_eq!(
        write_to(&confined(&dirs.workspace), &target),
        0,
        "the grant the operator wrote still works — if this fails, the profile \
         carries an unresolved path"
    );
    assert!(target.exists());
}

/// `2>/dev/null` works, which it did not before #212. A denied `/dev/null` would
/// surface as a missing `clang` via `xcode-select`, which hides the sandbox.
#[test]
fn a_confined_command_can_redirect_to_dev_null() {
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

/// Granting `/dev/null` must not grant `/dev`. Widening the rule to a subpath
/// would otherwise pass the test above unchanged.
#[test]
fn granting_dev_null_does_not_grant_the_device_directory() {
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

/// Network denial, judged by what the listener did not see. A closed port fails
/// the same way a denied connection does, so curl's exit code proves nothing.
#[test]
fn a_confined_command_cannot_reach_the_network() {
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

    let _ = confined(&dirs.workspace).exec("/usr/bin/curl", &args, None, None);
    assert!(
        connections.recv_timeout(Duration::from_secs(2)).is_err(),
        "the confined command reached a listener it should not have"
    );

    let _ = unconfined(&dirs.workspace).exec("/usr/bin/curl", &args, None, None);
    assert!(
        connections.recv_timeout(Duration::from_secs(5)).is_ok(),
        "the control didn't arrive either, so the case proved nothing — check \
         curl and the listener"
    );
}

/// The same denial, reached through a guest rather than the runner alone.
#[test]
fn a_guest_running_a_command_is_confined_too() {
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
        "the control wrote through the guest, so the denial below is from the sandbox"
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
