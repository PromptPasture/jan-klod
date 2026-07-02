# WIT interfaces — `jan-klod:interfaces@0.1.0`

Contracts between the core and WASM extensions. Standard tooling
(`wasm-tools`, `wit-bindgen-go`) discovers `.wit` files by scanning this
directory; no manifest file is required. External dependencies, if any, would
go in `deps.toml`.

## Files

| File | Kind | Interface(s) |
|---|---|---|
| `types.wit` | shared | `llm-types`, `store-types` |
| `extension-lifecycle.wit` | shared | `extension-lifecycle` (exported by every extension) |
| `llm-provider.wit` | extension-exported | `llm-provider` |
| `interceptor.wit` | extension-exported | `interceptor` (thin-loop decision hook; host-dispatched) |
| `memory-store.wit` | extension-exported | `memory-store` |
| `skill-registry.wit` | extension-exported | `skill-registry` |
| `mcp-registry.wit` | extension-exported | `mcp-registry` |
| `agent-delegate.wit` | extension-exported | `agent-delegate` |
| `tool-callable.wit` | extension-exported | `tool-callable` |
| `host-http.wit` | host-provided | `host-http` |
| `host-log.wit` | host-provided | `host-log` |
| `host-config.wit` | host-provided | `host-config` |
| `host-event.wit` | host-provided | `host-event` |
| `host-storage.wit` | host-provided | `host-storage` |
| `host-fs.wit` | host-provided | `host-fs` (path-jailed workspace read/write; default-deny) |

## Validate

```sh
# Install once
cargo install wasm-tools

# Parse + type-check the whole package
wasm-tools component wit wit/
```
