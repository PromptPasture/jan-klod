//! Component-Model bindings the core generates from the canonical `wit/`
//! contracts. We bind the `provider-world` because it is the richest world —
//! it imports every host capability the core implements (`host-log`,
//! `host-config`, `host-http`) and exports the universal `extension-lifecycle`.
//! The generated import traits are implemented in [`crate::host`]; the export
//! accessors are used once components are instantiated.

wasmtime::component::bindgen!({
    path: "../../../wit",
    world: "provider-world",
});
