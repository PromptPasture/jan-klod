# jan-klod

A sandboxed coding agent powered by the WebAssembly Component Model.

## What makes it different

Most agents run extensions in-process with full access. jan-klod isolates each as a WebAssembly component: WIT contracts limit imports, Wasmtime enforces fail-closed gates—no container needed.

Concretely: files → path-jailed workspace; commands → separate switch; network → opt-in; writes → confirmed (answer `always` to suppress for this run).

## Install

**No release yet.** Until the first tag, build from checkout:

```sh
git clone https://github.com/PromptPasture/jan-klod
cd jan-klod
make setup                  # once: pinned cargo plugins, git hooks, WIT deps
make bundle DIST=coding
```

Creates `dist/jan-klod-<version>-<os>-<arch>-coding.tar.gz` (same as release). Unpack; add `jan-klod` and `jan-klod-gateway` to PATH (`config.yaml` and `ext/` travel with them).

Below is the release-time installer (works after the first tag; now it has nothing to fetch):

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
```

## Quick start

```sh
export OPENAI_API_KEY=sk-...
jan-klod my-session
```

Shipped config enables `provider.openai` (hence the key). For Claude: export `ANTHROPIC_API_KEY` and toggle `extensions.provider.anthropic.enabled` (provider swap, no code change).

The arg is a session ID. `jan-klod` starts its own gateway (no port, nothing lingering). To run shared gateway:

```sh
jan-klod-gateway serve --bind 127.0.0.1:8787
jan-klod --addr 127.0.0.1:8787 my-session
```

No paths: gateway finds `config.yaml` and `ext/` in CWD or install dir (lets you `cd` to any repo). Positional names work only in checkouts.

Three surface modes (launch option, not install):

```sh
jan-klod my-session          # a line REPL
jan-klod tui my-session      # the full-screen terminal UI
jan-klod --gui               # the web client in a desktop window
```

`--gui` needs the archive (`install.sh --gui` or `make bundle GUI=1`); says so rather than fallback. Browser reaches `/` on gateway without install.

One-off answer, no server:

```sh
cd my-repo
jan-klod-gateway ask "what does this repo do?"
```

Stdout is answer only (pipes). Confirmations on terminal; stdin closed → default (refuse) — scripted `ask` reads only.

Diagnose issues:

```sh
jan-klod-gateway verify --live
```

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
