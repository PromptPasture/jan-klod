# jan-klod-core

The kernel: loads a `config.yaml`, builds a Wasmtime linker that grants each extension only the capabilities it declares, instantiates the enabled components from `ext/`, and runs the agent loop and its interceptor phases over them. It holds no domain logic; that lives in extensions.

Used as a library by [`jan-klod-host`](../host), which serves it over HTTP, WebSocket, ACP, MCP and Telegram.

```sh
cargo test -p jan-klod-core
cargo run -p jan-klod-core --features examples --example registry_index   # see scripts/registry-index.sh
```

Depends on [`jan-klod-config`](../config), [`jan-klod-protocol`](../protocol) and [`jk-session`](../session). Design: [architecture](../../../docs/concepts/architecture.md), [security model](../../../docs/concepts/security-model.md).
