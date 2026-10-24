//! Component-Model bindings for the `extension-world` in `wit/`.
//! Import traits are implemented in [`crate::host`].

wasmtime::component::bindgen!({
    path: "../../../wit",
    world: "extension-world",
});
