//! Load TinyGo `spike` component, call `complete`, echo result.
//! Runnable by hand; automated check via `tests/it/polyglot.rs`.

// bindgen!-generated code; exempt from workspace lints
#![allow(missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]

use wasmtime::component::{Component, Linker};
use wasmtime::{Engine, Result, Store};
use wasmtime_wasi::{ResourceTable, WasiCtx, WasiCtxBuilder, WasiCtxView, WasiView};

wasmtime::component::bindgen!({
    path: "../../wit/spike",
    world: "spike",
});

/// Host state. TinyGo wasip2 pulls in WASI modules; host must satisfy them
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
    let prompt = args
        .next()
        .unwrap_or_else(|| "hello, component model".to_string());

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
