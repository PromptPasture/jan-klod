//! Component-Model bindings the core generates from the canonical `wit/`
//! contracts. We bind the category-neutral `extension-world`: it imports every
//! host capability the core implements (`host-log`, `host-config`, `host-http`)
//! and exports only the universal `extension-lifecycle`, so a *single*
//! instantiation target drives lifecycle on any guest — a store, a provider, or
//! anything else — without the core depending on a category interface.
//! The generated import traits are implemented in [`crate::host`]; the export
//! accessors are used once components are instantiated.

wasmtime::component::bindgen!({
    path: "../../../wit",
    world: "extension-world",
});
