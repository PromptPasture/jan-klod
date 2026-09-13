//! Component-Model bindings from canonical `wit/` contracts.
//! Binds category-neutral `extension-world`: imports host capabilities
//! (`host-log`, `host-config`, `host-http`), exports universal `extension-lifecycle`
//! for single instantiation target across any guest. Import traits in [`crate::host`];
//! export accessors used after instantiation.

wasmtime::component::bindgen!({
    path: "../../wit",
    world: "extension-world",
});
