//! A component built from another language runs in this host.
//!
//! "Any language" is one of the four things this runtime claims, and the only
//! evidence for it was `examples/spike_gate.rs` — a binary someone had to run by
//! hand. Two documents state that the TinyGo spike is "the standing polyglot
//! canary (`make gate`)". `make gate` did not run it; the Makefile says plainly a
//! few lines away that the spike "stays opt-in: `make spike-guest`". A claim
//! nobody executes is the same shape as a denylist nobody updates.
//!
//! So the round-trip is a test now. It loads the committed `spike.wasm` — a
//! reactor component TinyGo built against `wit/spike` — instantiates it in
//! Wasmtime with the host's own WASI wiring, calls its exported `complete`, and
//! checks the answer came back across the boundary. Every gate run, no toolchain
//! required.
//!
//! **What this does and does not prove.** It proves the host can load and call a
//! component that was not built from Rust, which is the load-bearing half: the
//! Component Model boundary is genuinely language-neutral rather than a Rust ABI
//! with extra steps. It does *not* prove the current `wit/` still compiles under
//! TinyGo, because the artifact is committed rather than rebuilt — that needs
//! `tinygo` and `wkg`, and requiring them would make the gate unrunnable for most
//! people. [`the_committed_artifact_is_rebuildable`] closes that half when the
//! toolchain happens to be present, and says so when it is not.

// Dominated by `bindgen!` output; exempt from the workspace's doc/style lints.
#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use std::path::PathBuf;

use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

mod common;

wasmtime::component::bindgen!({
    path: "../../../wit/spike",
    world: "spike",
});

/// Even though the `spike` world declares no imports, a TinyGo `wasip2`
/// component pulls in `wasi:cli`/`wasi:io` for its runtime, so the host has to
/// satisfy those. That is itself part of what is being checked: a guest from
/// another toolchain arrives with its own runtime expectations.
struct Host {
    ctx: WasiCtx,
    table: ResourceTable,
}

impl WasiView for Host {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView { ctx: &mut self.ctx, table: &mut self.table }
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
        Host { ctx: WasiCtxBuilder::new().inherit_stdio().build(), table: ResourceTable::new() },
    );

    let spike = Spike::instantiate(&mut store, &component, &linker).expect("instantiates");
    let echoed = spike.call_complete(&mut store, "hello, component model").expect("the call returns");

    // The guest's own logic, run in our sandbox: `"echo: " + prompt`, written in Go.
    assert_eq!(
        echoed, "echo: hello, component model",
        "a value crossed into a Go component and came back changed by its code"
    );
}

/// The other half: does today's `wit/` still produce a working Go guest?
///
/// Only checkable where `tinygo` and `wkg` exist. That is a genuinely optional
/// toolchain — CI does not carry it and most contributors will not install it —
/// so this skips via [`common::optional_tool`] rather than the `JK_REQUIRE_GUESTS`
/// policy: making it a hard failure would teach people to unset the flag, which
/// costs more than it buys. The skip announces itself.
///
/// It matters because without it the committed artifact can drift from a contract
/// it no longer matches, and the test above would keep passing against a fossil.
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
