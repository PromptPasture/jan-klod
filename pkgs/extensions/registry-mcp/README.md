# registry-mcp

MCP gateway: connects to SSE MCP servers and proxies tool calls via mcp-registry WIT.

A `registry` extension: a WebAssembly component built against [`wit/`](../../../wit).

```sh
make -C pkgs/extensions all   # builds every guest and stages ext/registry-mcp.wasm with its manifest
```

The capabilities it needs are read from the built component into `ext/registry-mcp.manifest.toml`; they are not listed here.
