//! A component built from another language runs in this host — the "any
//! language" claim, backed by an actual test rather than a spike binary someone
//! had to run by hand.
//!
//! Loads the committed `spike.wasm` (TinyGo, built against `wit/spike`),
//! instantiates it with the host's own WASI wiring, calls its exported
//! `complete`, and checks the round trip. Runs every gate, no toolchain
//! required.
//!
//! This proves the Component Model boundary is genuinely language-neutral, not
//! that today's `wit/` still compiles under TinyGo — the artifact is committed,
//! not rebuilt, since that needs `tinygo`/`wkg` most contributors lack.
//! [`the_committed_artifact_is_rebuildable`] covers that half when the toolchain
//! is present.

// Dominated by `bindgen!` output; exempt from the workspace's doc/style lints.
#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use std::path::PathBuf;

use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

use crate::common;

wasmtime::component::bindgen!({
    path: "../../../wit/spike",
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

#[test]
fn a_component_built_from_go_completes_a_call_through_the_host() {
    let path = spike_wasm();
    assert!(
        path.exists(),
        "the committed TinyGo component is missing at {} — the polyglot claim has \
         no evidence without it",
        path.display()
    );

    let engine = Engine::default();
    let component = Component::from_file(&engine, &path).expect("the Go-built component compiles");

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
    let echoed = spike
        .call_complete(&mut store, "hello, component model")
        .expect("the call returns");

    // The guest's own logic, run in our sandbox: `"echo: " + prompt`, written in Go.
    assert_eq!(
        echoed, "echo: hello, component model",
        "a value crossed into a Go component and came back changed by its code"
    );
}

/// The other half: does today's `wit/` still produce a working Go guest? Only
/// checkable where `tinygo`/`wkg` exist, so this skips via
/// [`common::optional_tool`] (an announced skip) rather than the
/// `JK_REQUIRE_GUESTS` policy — forcing a hard failure on a toolchain most
/// contributors lack would just teach people to unset the flag.
#[test]
fn the_committed_artifact_is_rebuildable() {
    if !common::optional_tool("tinygo") || !common::optional_tool("wkg") {
        return;
    }
    let extensions = common::repo_root().join("src/extensions");
    let output = std::process::Command::new("make")
        .arg("spike-guest")
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
    assert!(spike_wasm().exists(), "the rebuild produced the component");
}
