//! A TinyGo component answers a call through the committed fixture `pkgs/extensions/spike/spike.wasm`; needs no toolchain.
//! Rebuild the fixture (`make -C pkgs/extensions spike-guest`) only when committing a new one.

#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use std::path::{Path, PathBuf};

use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::common;

wasmtime::component::bindgen!({
    path: "../../../wit/spike",
    world: "spike",
});

/// TinyGo `wasip2` components still import `wasi:cli`/`wasi:io`, so the host must link WASI.
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
    common::repo_root().join("pkgs/extensions/spike/spike.wasm")
}

/// Instantiates the component at `path` and calls its `complete`.
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
         pkgs/extensions spike-guest`) if committing a new one",
        path.display(),
        path.display()
    );

    assert_eq!(
        echo_through_the_host(&path),
        "echo: hello, component model",
        "a value crossed into a Go component and came back changed by its code"
    );
}

/// Checks that today's `wit/` still yields a working Go guest. Skips without
/// `tinygo`/`wkg`. Rebuilds into a temp dir, never over the fixture, and does not
/// assert byte equality since TinyGo output varies by version.
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

    let extensions = common::repo_root().join("pkgs/extensions");
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
