//! Component-Model bindings for the `extension-world` in `wit/`.

wasmtime::component::bindgen!({
    path: "../../../wit",
    world: "extension-world",
});
