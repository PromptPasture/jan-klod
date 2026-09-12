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

**Nothing has shipped yet — there is no release to download.** Until the first
tag, build from a checkout:

```sh
git clone https://github.com/PromptPasture/jan-klod
cd jan-klod
make setup                  # once: pinned cargo plugins, git hooks, WIT deps
make bundle DIST=coding
```

That writes `dist/jan-klod-<version>-<os>-<arch>-coding.tar.gz`, the same archive
a release would publish. Unpack it and put `jan-klod` and `jan-klod-gateway` on
your PATH; `config.yaml` and `ext/` travel with them.

The installer below is the path once there is a release. Run today it reports
`could not determine latest release tag` and exits non-zero — it is correct, it
simply has nothing to fetch yet.

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

The argument is a session id. `jan-klod` starts its own gateway and talks to it
over a pipe — no port, nothing left running. To run one yourself and share it
between clients:

```sh
jan-klod-gateway serve --bind 127.0.0.1:8787
jan-klod --addr 127.0.0.1:8787 my-session
```

No paths: the gateway finds `config.yaml` and `ext/` in the current directory, or
in its own installed data directory otherwise. That is what lets you `cd` into any
repository and run it there. (Naming them positionally still works, but only
inside a checkout, where they happen to be beside you.)

The same client, three ways to look at it — the surface is a launch mode, not a
different install:

```sh
jan-klod my-session          # a line REPL
jan-klod tui my-session      # the full-screen terminal UI
jan-klod --gui               # the web client in a desktop window
```

`--gui` needs the archive that carries the window
(`install.sh --gui`, or `make bundle GUI=1` from a checkout); it says so rather
than falling back to the terminal. A browser reaches the same page at the
gateway's `/` with nothing installed at all.

For a single answer, with no server to leave running:

```sh
cd my-repo
jan-klod-gateway ask "what does this repo do?"
```

stdout is the answer and nothing else, so it pipes. Confirmations are asked on the
terminal; with stdin closed they take the default, which is a refusal — so a
scripted `ask` reads and searches but will not write.

If something is not working, one command says why — it starts every extension and
then asks the model one question:

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
