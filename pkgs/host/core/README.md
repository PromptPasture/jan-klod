# jan-klod-core

The harness: boots the component runtime ([`jk-wasm`](../wasm)) and the session store ([`jk-session`](../session)), and runs the agent loop and its interceptor phases over them. It holds no domain logic; that lives in extensions.

Used as a library by [`jan-klod-host`](../host), which serves it over HTTP, WebSocket, ACP, MCP and Telegram.

```sh
cargo test -p jan-klod-core
```

Depends on [`jan-klod-protocol`](../protocol), [`jk-session`](../session) and [`jk-wasm`](../wasm). Design: [architecture](../../../docs/concepts/architecture.md), [security model](../../../docs/concepts/security-model.md).
