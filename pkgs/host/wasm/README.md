# jk-wasm

The WebAssembly boundary: loads a `config.yaml`, builds a Wasmtime linker that grants each extension only the capabilities it declares, compiles the enabled components from `ext/`, and adapts them to the traits the loop consumes. It also holds the capability hosts (`host-fs`, `host-process`, `host-http`), the effect sandbox, and the `ext` install path. The only crate in the tree that names `wasmtime`.

Used by [`jan-klod-core`](../core) and [`jan-klod-host`](../host). Depends on [`jan-klod-config`](../config) and [`jan-klod-protocol`](../protocol).

```sh
cargo test -p jk-wasm
cargo run -p jk-wasm --features examples --example registry_index   # see scripts/registry-index.sh
```

Design: [architecture](../../../docs/concepts/architecture.md), [security model](../../../docs/concepts/security-model.md).
