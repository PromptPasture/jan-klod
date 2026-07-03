# jan-klod

A sandboxed coding agent powered by the WebAssembly Component Model.

## What makes it different

Most coding agents run extensions (tools, providers, interceptors) in the same
process with full system access. jan-klod runs each extension as an isolated
WebAssembly component: typed WIT contracts define exactly what it can import,
and the Wasmtime host enforces fail-closed permission gates — without a container.

## Install

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
```

## Quick start

```sh
export ANTHROPIC_API_KEY=sk-ant-...
jan-klod serve config.yaml ext
# in another terminal:
jan-klod-ui 127.0.0.1:8787 my-session
```

See the [quickstart guide](docs/quickstart.md) for a full walkthrough.

## Architecture

- **Host** — Rust + Wasmtime; owns the session loop, HTTP surface, and SQLite store.
- **Extensions** — WebAssembly components compiled to `wasm32-wasip2`:
  - `provider-anthropic` / `provider-openai` — LLM providers
  - `tool-fs` / `tool-shell` — sandboxed file and shell tools
  - `registry-skills` — named skill shortcuts from `.agents/skills/`
  - `registry-mcp` — SSE MCP server gateway
  - `interceptor-*` — typed hooks into the conductor pipeline
- **WIT contracts** (`wit/`) — the interfaces extensions implement and host capabilities they may import.

## Documentation

- [Quickstart](docs/quickstart.md)
- [Concepts](docs/concepts/)
- [Architecture decisions](docs/decisions/)
- [Roadmap](docs/concepts/roadmap.md)

## License

Apache-2.0 — see [LICENSE](LICENSE).
