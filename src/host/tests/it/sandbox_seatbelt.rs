//! What a confined command can and cannot do, on macOS.
//!
//! The three behaviours Slice 15b exists for — an escape write is denied, an
//! in-workspace write is allowed, the network is refused — driven through the
//! real [`ProcessRunner`], plus through Component-Model boundary to cover the
//! guest's path.
//!
//! **The in-workspace write is not a formality.** Seatbelt matches the resolved
//! path, and every temp directory here is under a symlink (`/var` →
//! `/private/var`), so a profile carrying the unresolved path denies *everything*
//! — including the grant. A suite with only the escape test would be green and
//! hollow. That case fails if canonicalization is ever dropped.
//!
//! macOS-only by `#[cfg]`, because Seatbelt is. On Linux these don't exist,
//! which [#95](https://github.com/PromptPasture/jan-klod/issues/95) records:
//! CI runs on Linux, so nothing here is verified there.
#![cfg(target_os = "macos")]

use std::io::Read;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use jan_klod_core::host_fs::Workspace;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::sandbox::{self, SandboxMode, SandboxPolicy};
use jan_klod_core::tool_host::ToolExtension;
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

/// The same runner with nothing confining it — the control for every case
/// below. Without it, a failure proves nothing — a missing binary or bad cwd
/// fails identically.
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

    // Control first: unconfined, this succeeds. That makes the denial below
    // attributable to the sandbox.
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

/// The case that catches canonicalization bugs. See the module docs.
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

/// `2>/dev/null` works, which it did not before #212.
///
/// The shell opens the redirect *before* running the program, so a denied
/// `/dev/null` means the command never executes — and the error names
/// `/dev/null` rather than anything the operator wrote. This was found through
/// a cascade: `xcode-select` probes with `2> /dev/null`, so a refused write
/// here was reported as a missing `clang`, which reads like a broken
/// toolchain rather than a sandbox.
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

/// The other half of the one above, and the reason it is not a hole.
///
/// Granting `/dev/null` must grant `/dev/null` and not `/dev`. Without this,
/// widening the rule from `literal` to `subpath` — an easy edit, since every
/// other grant in the profile is a `subpath` — would hand every command the
/// device directory and pass the test above unchanged.
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

/// Network denial, asserted by what the listener *did not see*.
///
/// Not by `curl`'s exit code: a connection to a closed port fails the same way
/// a denied one does. A listener that is genuinely accepting connections tells
/// the two apart — the unconfined attempt arrives, the confined one does not.
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
            // Read briefly so the client isn't left writing into a void, then
            // report that someone arrived.
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

/// The same denial, reached the way a component reaches it.
///
/// `host_process.rs` drives this guest through an unconfined runner; this drives
/// it through a confined one, so what is covered is the guest's path to a
/// *sandboxed* command rather than the runner's alone.
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

    // The control, through the same guest: unconfined, the write lands.
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
