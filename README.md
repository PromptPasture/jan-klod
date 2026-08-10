# jan-klod

A sandboxed coding agent powered by the WebAssembly Component Model.

## What makes it different

Most coding agents run extensions (tools, providers, interceptors) in the same
process with full system access. jan-klod runs each extension as an isolated
WebAssembly component: typed WIT contracts define exactly what it can import,
and the Wasmtime host enforces fail-closed permission gates — without a container.

Concretely: a file tool gets a path-jailed workspace and nothing else; running a
command needs a separate deployment switch; a tool has no network unless its entry
asks for it; and every write is confirmed with you before it happens (answer
`always` to stop being asked for that kind of action, for this run only).

## Install

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
```

## Quick start

```sh
export OPENAI_API_KEY=sk-...
jan-klod my-session
```

The shipped `config.yaml` enables `provider.openai`, which is why that is the key
to set. For native Claude, export `ANTHROPIC_API_KEY` instead and flip
`extensions.provider.anthropic.enabled: true` (and `openai` to `false`) — the
provider is a component swap, not a code change.

`jan-klod` starts the gateway automatically. To run it yourself:

```sh
jan-klod-gateway serve --bind 127.0.0.1:8787
```

No paths: the gateway finds `config.yaml` and `ext/` in the current directory, or
in its own installed data directory otherwise. That is what lets you `cd` into any
repository and run it there. (Naming them positionally still works, but only
inside a checkout, where they happen to be beside you.)

For a single answer, with no server to leave running:

```sh
cd my-repo
jan-klod-gateway ask "what does this repo do?"
```

stdout is the answer and nothing else, so it pipes. Confirmations are asked on the
terminal; with stdin closed they take the default, which is a refusal — so a
scripted `ask` reads and searches but will not write.

See the [quickstart guide](docs/quickstart.md) for a full walkthrough, and the
[security model](docs/concepts/security-model.md) for what each capability is
granted and which test proves it.

## Architecture

- **Host** — Rust + Wasmtime; owns the session loop, HTTP surface, and SQLite store.
- **Extensions** — WebAssembly components compiled to `wasm32-wasip2`:
  - `provider-openai` / `provider-anthropic` — LLM providers
  - `tool-fs` / `tool-edit` / `tool-find` — read, anchored partial edits, glob
  - `tool-git` — read-only repository inspection (no `commit`/`push` exists in it)
  - `tool-shell` / `tool-fetch` — command execution and URL retrieval, both off by default
  - `registry-skills` — named skill shortcuts from `.agents/skills/`
  - `registry-mcp` — SSE MCP server gateway
  - `interceptor-*` — the loop's decisions: intent, context, tool selection,
    permission, system prompt
- **WIT contracts** (`wit/`) — the interfaces extensions implement and host capabilities they may import.

## Documentation

- [Quickstart](docs/quickstart.md)
- [Concepts](docs/concepts/)
- [Architecture decisions](docs/decisions/)
- [Roadmap](docs/concepts/roadmap.md)

## License

Apache-2.0 — see [LICENSE](LICENSE).
