//! Slice 1a gate host: load a TinyGo-built `spike` component, call its exported
//! `complete` function across the Component Model boundary, print the echo.
//!
//! Synchronous Wasmtime on purpose — see the async-model decision in
//! `docs/decisions/2026-06-29-extension-technologies/`. This whole binary is a
//! throwaway gate; delete with `src/extensions/spike/` once the verdict lands.

use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Result, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../../../wit/spike",
    world: "spike",
});

/// Store state. Even though the `spike` world declares no imports, a TinyGo
/// `wasip2` component pulls in `wasi:cli`/`wasi:io`/etc. for its runtime, so the
/// host must satisfy those via `wasmtime-wasi`.
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

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let component_path = args
        .next()
        .unwrap_or_else(|| "../extensions/spike/spike.wasm".to_string());
    let prompt = args.next().unwrap_or_else(|| "hello, component model".to_string());

    let engine = Engine::default();
    let component = Component::from_file(&engine, &component_path)
        .map_err(|e| e.context(format!("loading component {component_path}")))?;

    let mut linker: Linker<Host> = Linker::new(&engine);
    wasmtime_wasi::p2::add_to_linker_sync(&mut linker)?;

    let mut store = Store::new(
        &engine,
        Host {
            ctx: WasiCtxBuilder::new().inherit_stdio().build(),
            table: ResourceTable::new(),
        },
    );

    let spike = Spike::instantiate(&mut store, &component, &linker)?;
    let echo = spike.call_complete(&mut store, &prompt)?;

    println!("{echo}");
    Ok(())
}
