//! A component built from another language runs in this host — the "any
//! language" claim, backed by an actual test rather than a spike.
//!
//! Loads `src/extensions/spike/spike.wasm` (TinyGo, built against `wit/spike`),
//! instantiates it with the host's WASI wiring, calls its exported `complete`,
//! and checks the round trip. Runs every gate, no toolchain required.
//!
//! That artifact is a **committed fixture**, the one exception to ignored guest
//! components: `.gitignore` negates it, `make -C src/extensions clean` leaves it
//! alone, and `make -C src/extensions spike-guest` regenerates it with `-no-debug
//! -opt=z` (~75 KB, no DWARF) for whoever commits the new one. Nothing in a test
//! run writes to it.
//!
//! This proves the Component Model boundary is language-neutral, not that today's
//! `wit/` still compiles under TinyGo — the committed artifact is what runs.
//! [`the_committed_artifact_is_rebuildable`] covers rebuilding when the toolchain
//! is present, building into a temp dir so the fixture stays untouched.

// Dominated by `bindgen!` output; exempt from the workspace's doc/style lints.
#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use std::path::{Path, PathBuf};

use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::common;

wasmtime::component::bindgen!({
    path: "../../wit/spike",
    world: "spike",
});

/// The `spike` world declares no imports, but a TinyGo `wasip2` component still
/// pulls in `wasi:cli`/`wasi:io` for its runtime, which the host must satisfy.
struct Host {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.ctx,
            table: &mut self.table,
        }
    }
}

fn spike_wasm() -> PathBuf {
    common::repo_root().join("src/extensions/spike/spike.wasm")
}

/// Instantiate the component at `path` in this host and call its `complete`.
/// Shared by both tests so each asserts the same round trip — one against the
/// committed fixture, one against a fresh build of it.
fn echo_through_the_host(path: &Path) -> String {
    let engine = Engine::default();
    let component = Component::from_file(&engine, path).expect("the Go-built component compiles");

    let mut linker: Linker<Host> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker).expect("wasi is wired");
    let mut store = Store::new(
        &engine,
        Host {
            ctx: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
        },
    );

    let spike = Spike::instantiate(&mut store, &component, &linker).expect("instantiates");
    spike
        .call_complete(&mut store, "hello, component model")
        .expect("the call returns")
}

#[test]
fn a_component_built_from_go_completes_a_call_through_the_host() {
    let path = spike_wasm();
    assert!(
        path.exists(),
        "the committed TinyGo component is missing at {} — restore it with \
         `git checkout -- {}` rather than rebuilding; only rebuild (`make -C \
         src/extensions spike-guest`) if committing a new one",
        path.display(),
        path.display()
    );

    // The guest's own logic, run in our sandbox: `"echo: " + prompt`, written in Go.
    assert_eq!(
        echo_through_the_host(&path),
        "echo: hello, component model",
        "a value crossed into a Go component and came back changed by its code"
    );
}

/// The other half: does today's `wit/` still produce a working Go guest? Only
/// checkable where `tinygo`/`wkg` exist, so this skips via
/// [`common::optional_tool`] (an announced skip) rather than the
/// `JK_REQUIRE_GUESTS` policy.
///
/// Rebuilds into a temp dir via `SPIKE_WASM`, never over the committed fixture:
/// writing there made test outcomes depend on test order and left `make gate`
/// with a dirty tree. The fresh component is put through the same round trip,
/// catching drift that compiles but no longer works. Byte equality is not
/// asserted — TinyGo output moves with the compiler version.
#[test]
fn the_committed_artifact_is_rebuildable() {
    if !common::optional_tool("tinygo") || !common::optional_tool("wkg") {
        return;
    }
    let committed = std::fs::read(spike_wasm()).expect("the committed fixture is readable");

    let dir = std::env::temp_dir().join(format!("jk-polyglot-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let rebuilt = dir.join("spike.wasm");

    let extensions = common::repo_root().join("src/extensions");
    let output = std::process::Command::new("make")
        .arg("spike-guest")
        .arg(format!("SPIKE_WASM={}", rebuilt.display()))
        .current_dir(&extensions)
        .output()
        .expect("make runs");

    assert!(
        output.status.success(),
        "the TinyGo guest no longer builds against wit/ — the contracts have drifted \
         away from what a non-Rust toolchain can express:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(rebuilt.exists(), "the rebuild produced the component");
    assert_eq!(
        echo_through_the_host(&rebuilt),
        "echo: hello, component model",
        "the guest rebuilt from today's wit/ still answers across the boundary"
    );
    assert_eq!(
        std::fs::read(spike_wasm()).expect("the committed fixture is still readable"),
        committed,
        "rebuilding must not touch the committed fixture — that is what dirtied the \
         working tree and coupled these two tests"
    );
}
