//! What a confined command can and cannot do, on Linux.
//!
//! The same four cases as `sandbox_seatbelt.rs`, deliberately — two backends
//! confining the same policy should be held to one standard, and a reader
//! comparing the files should see the mechanism differ and nothing else. Each
//! runs its command **unconfined first**, because a missing binary, a bad cwd or
//! a typo in the test fails exactly the way a denial does.
//!
//! # Two things this module asserts that 15b's does not have to
//!
//! **The kernel is asked first.** Landlock is a kernel feature and can simply be
//! absent (pre-5.13, or built without `CONFIG_SECURITY_LANDLOCK`), whereas
//! `sandbox-exec` ships with every macOS. Without that check a kernel that
//! cannot enforce anything would make the *escape* tests pass — the write fails
//! because the wrapper refuses to run at all — while the in-workspace case fails
//! confusingly. `available()` turns that into one clear message.
//!
//! **The wrapper is named explicitly.** `LandlockBackend` confines by
//! re-executing the gateway, and under `cargo test` `current_exe()` is the *test
//! binary*, which has no `confine` subcommand. Every command would then fail in
//! a way that reads exactly like Landlock refusing. So these tests point the
//! backend at `CARGO_BIN_EXE_jan-klod-gateway`, which is the seam
//! `LandlockBackend::new` exists for.
//!
//! Linux-only by `#[cfg]`, because Landlock is. Unlike 15b's macOS tests, CI
//! *does* run these — `ubuntu-latest` has Landlock — so this is the half of
//! Phase 15 that a regression cannot slip past
//! ([#95](https://github.com/PromptPasture/jan-klod/issues/95) is the other
//! half).
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

/// A workspace and a sibling outside it, removed on drop.
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

/// Fails the test with one clear message when the kernel cannot enforce
/// anything, rather than letting the denial cases pass for the wrong reason.
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

/// A runner confined by the gateway this test build produced — see the module
/// docs for why the path is named rather than discovered.
fn confined(workspace: &Path) -> ProcessRunner {
    let backend = LandlockBackend::new(PathBuf::from(env!("CARGO_BIN_EXE_jan-klod-gateway")));
    runner(workspace).with_sandbox(std::sync::Arc::new(backend), policy(workspace))
}

/// The control for every case below.
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

/// The case that catches a *broken* sandbox rather than a missing one: if the
/// wrapper cannot be found or the ruleset grants nothing, this is what fails.
#[test]
fn a_confined_command_can_still_write_inside_the_workspace() {
    require_landlock();
    let dirs = dirs("inside");
    let target = dirs.workspace.join("ok");
    assert_eq!(
        write_to(&confined(&dirs.workspace), &target),
        0,
        "the grant the operator wrote still works — if this fails, either the wrapper was \
         not reached or the ruleset is not granting what it was told to"
    );
    assert!(target.exists());
}

/// Network denial, asserted by what the listener did not see — a connection to
/// a closed port fails the same way a denied one does.
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
