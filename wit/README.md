# WIT interfaces — `jan-klod:interfaces@0.1.0`

Contracts between core and WASM extensions. Tooling discovers `.wit` by scanning this dir (no manifest); external deps in `deps.toml`.

## Files

| File | Kind | Interface(s) |
|---|---|---|
| `types.wit` | shared | `llm-types`, `store-types` |
| `extension-lifecycle.wit` | shared | `extension-lifecycle` (all extensions export) |
| `llm-provider.wit` | extension-exported | `llm-provider` |
| `interceptor.wit` | extension-exported | `interceptor` (loop decision hook; host-dispatched) |
| `skill-registry.wit` | extension-exported | `skill-registry` |
| `mcp-registry.wit` | extension-exported | `mcp-registry` |
| `agent-delegate.wit` | extension-exported | `agent-delegate` |
| `tool-callable.wit` | extension-exported | `tool-callable` |
| `host-http.wit` | host-provided | `host-http` |
| `host-log.wit` | host-provided | `host-log` |
| `host-config.wit` | host-provided | `host-config` |
| `host-event.wit` | host-provided | `host-event` |
| `host-storage.wit` | host-provided | `host-storage` |
| `host-fs.wit` | host-provided | `host-fs` (workspace R/W; default-deny) |
| `host-process.wit` | host-provided | `host-process` (run-to-completion; default-deny) |

## Validate

```sh
cargo install wasm-tools         # once
wasm-tools component wit wit/    # parse + type-check
```

## Doc comments: no double quotes

`"` in `///` breaks *generated Rust* (not WIT). `wit-bindgen` emits to Rust doc; lexer reports error at `generate!`, far from cause. Use backticks.
