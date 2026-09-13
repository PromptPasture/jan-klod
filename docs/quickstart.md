---
title: Quickstart
description: Install jan-klod, start the server, and run your first session in under 5 minutes.
---

# Quickstart

## 1. Install

**No release yet**, so build from checkout:

### Build from source

```sh
git clone https://github.com/PromptPasture/jan-klod
cd jan-klod
make setup                  # once: pinned cargo plugins, git hooks, WIT deps
make bundle DIST=coding
```

`make bundle` produces `dist/jan-klod-<version>-<os>-<arch>-coding.tar.gz`, with `jan-klod`, `jan-klod-gateway`, pre-filled `config.yaml`, and `ext/`. Unpack it and put the binaries on your PATH. `DIST` takes `coding`, `headless-chat`, or `minimal`; add `GUI=1` for the desktop window.

For development in the checkout, see [development setup](guides/development-setup.md).

### After first release: installer

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
```

That gives **`coding`** — models, interceptor set, and file and git tools. Two other distributions exist; a distribution is about what the install is *for*, not which client it carries:

```sh
# a chat channel over Telegram; nothing that touches the machine
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh -s -- --dist headless-chat

# one provider and the interceptor set, and nothing else
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh -s -- --dist minimal
```

[What each one carries](concepts/configurator.md#distributions).

That installs `jan-klod` (TUI) and `jan-klod-gateway` (server) to `~/.local/bin`. Add to your PATH if needed:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

### Verifying what you downloaded

`install.sh` checks against `SHA256SUMS.txt` from the same release. **That proves the download wasn't corrupted, not that the bundle is ours** — checksums travel with the bundle, so whoever can serve one can serve both.

A minisign signature closes that by being published where the release cannot rewrite it. Verify with one command:

```sh
minisign -Vm jan-klod-<version>-<os>-<arch>-coding.tar.gz -P <public key>
```

**No release has been signed yet** — see [#93](https://github.com/PromptPasture/jan-klod/issues/93). The public key appears on the [landing page](https://promptpasture.github.io/jan-klod/) and in the repository once the first signed release exists. Until then, `registry.trusted-keys` in `config.yaml` ships empty, so `ext install` refuses first-party components as untrusted.

## 2. Set your API key

jan-klod ships with an Anthropic provider (native Claude) and an OpenAI-compatible provider.

**Anthropic (recommended):**

```sh
export ANTHROPIC_API_KEY=sk-ant-...
```

Then edit `config.yaml` and set `extensions.provider.anthropic.enabled: true` and `extensions.provider.openai.enabled: false`.

**OpenAI or compatible:**

```sh
export OPENAI_API_KEY=sk-...
```

The default `config.yaml` has `provider.openai` enabled.

## 3. Start a session

From inside a repository you want to work on:

```sh
cd ~/my-project
jan-klod my-session
```

`jan-klod` starts its own `jan-klod-gateway rpc` and talks over stdin/stdout — no port, no token, nothing left running on exit. The gateway uses the current directory as workspace, jailing file tools to it.

The single argument is the **session id**. To drive a gateway that is already listening somewhere, name it with `--addr`:

```sh
jan-klod --addr 127.0.0.1:8787 my-session
```

### The same client, in a window

```sh
jan-klod --gui
```

This spawns or attaches a gateway, then opens the core's web client in a desktop window using the system webview. No bundled browser, no separate GUI codebase: it's the **same** page a browser gets at `/`, so anything true of one is true of the other. The token comes across, so no retype.

`--gui` takes `--addr` like other modes and **no session id** — the page chooses its own, refusing if one is passed.

The window ships only in the `-gui` archive (`install.sh --gui`, or `make bundle GUI=1` from checkout). Without it, `--gui` says the shell isn't installed and exits non-zero. On Linux, install `libwebkit2gtk-4.1-0` and `libayatana-appindicator3-1` if the window won't open. On macOS, it's built in.

To run the gateway as a background service or share between clients:

```sh
jan-klod-gateway serve --bind 127.0.0.1:8787
```

Or speak the client protocol on a pipe (what `jan-klod` does and what an editor would do):

```sh
jan-klod-gateway rpc
```

It reads newline-delimited JSON-RPC 2.0 on stdin and writes it on stdout;
**stdout carries nothing else**, so logs and errors go to stderr.

## Point another agent at it (MCP)

Claude Code, Codex, Goose, and editors consume MCP servers. `jan-klod` is one:

```sh
jan-klod-gateway mcp
```

Same pipe discipline as `rpc` — client spawns it, frames on stdout, logs on stderr. Three tools: `ask` (one turn), `session_list`, `session_get`. Point a client at it the way you point at any stdio MCP server; for Claude Code:

```sh
claude mcp add jan-klod -- jan-klod-gateway mcp
```

Run it from the repository you want it to work on — the gateway uses the current directory as the workspace, so file tools are jailed to it.

**MCP turns won't write or run commands.** No one to answer confirmations, so each takes its denying default — same rule as scripted `ask`. Reads and searches work; writes return refusals the model can see and explain. For writes, drive it from `jan-klod` or REST, where someone can say yes.

## One question, no server

For a single answer in a shell or CI step, nothing stays running:

```shell
cd my-repo
jan-klod-gateway ask "what does this repo do?"
```

The answer goes to stdout only, so it pipes. Confirmations prompt the terminal if one exists; with stdin closed (pipe, CI job) each takes its denying default — so scripted `ask` reads and searches but won't write or run unless someone is there to say yes.

Type a message and press **Enter** to send it. The model's response streams in token by token. Press **Esc** to quit.

## 4. Ask it to do something

```
> Read src/main.rs and summarize what it does
> Add a --verbose flag to the CLI
> Run the tests and fix any failures
```

File reads, writes, and shell commands go through permission-gated sandboxed extensions. Enabled by default: `tool.fs` (read/write/grep — grep searches the workspace tree), `tool.edit` (partial edits), and `tool.find` (glob the workspace, e.g. `**/*.rs`).

Command execution is off: set `execution.enabled: true` in `config.yaml`, then enable the tool — `tool.git` (read-only: `status`/`diff`/`log`/`show`/`branch`, no repo modifications) or `tool.shell` (any command). The `permission` interceptor (on by default) confirms every write and command; turn it off to skip the prompt.

The TUI shows a question with accepted answers; Enter answers the waiting turn instead of sending a new message (empty line takes the default). Answer `yes`/`no` for that call, or `always`/`never` to decide for that action class — `fs:write`, `shell:cargo` — for the run. Standing decisions are in-memory only: restart asks again. They never cover arguments outside the workspace, so "always allow writes" never permits `/etc/passwd`.

`tool.edit` changes part of a file instead of rewriting it. It shows the model each line with an **anchor** (a hash of position and text), and edits name those anchors. If the file changed, anchors no longer match and the edit is refused rather than applied to wrong lines.

## 5. Reach it from another machine

The gateway binds `127.0.0.1` by default, unset, with no authentication — fine for loopback, not elsewhere. To expose it:

```sh
export JAN_KLOD_TOKEN=$(openssl rand -hex 32)
jan-klod-gateway serve --bind 0.0.0.0:8787
```

Every route except `/health` requires `Authorization: Bearer <token>`. Set the same variable where you run `jan-klod` and the client sends it. No TLS: put it behind something that terminates it if the network isn't trusted.

## 6. Check the install

If a tool seems missing, ask the runtime what it loaded:

```sh
jan-klod-gateway verify --live
```

It resolves every enabled extension, reports anything absent from `ext/`, then starts them all — exiting non-zero if any part is incomplete. Release bundles go through the same check.

`--live` then asks the model one question. That matters: everything above is offline and passes with wrong API key, unreachable local server, nonexistent model, or refused `base-url` — the whole list of first-run gotchas. Drop `--live` in build steps; keep it when asking "why doesn't this work?"

To see what's in `ext/` and what each component may ask the host for — which `ls` won't tell you:

```sh
jan-klod-gateway ext list
```

A component from elsewhere goes in with `ext install`, which verifies before copying — from a path or URL:

```sh
jan-klod-gateway ext install ./tool-thing.wasm --sha256 <hex> --allow-unsigned
jan-klod-gateway ext install https://example.com/ext/tool-thing.wasm
```

A URL names the component; the manifest and signatures fetch from beside it. Only public destinations are allowed, checked before fetching and on every redirect.

Signed is the default; `--allow-unsigned` deliberately bypasses it and requires `--sha256`, since waiving the signature leaves the digest as the only evidence. Name trusted keys in `registry.trusted-keys` and the flags become unnecessary — see [Configuration → Putting something in `ext/`](concepts/configuration.md#putting-something-in-ext-registry). Refused installs leave `ext/` unchanged.

## 7. Resume a session

Sessions persist across restarts (SQLite store). The REST API:

```sh
# List past sessions
curl http://127.0.0.1:8787/sessions

# Create a named session explicitly
curl -X POST http://127.0.0.1:8787/sessions

# Get a session transcript
curl http://127.0.0.1:8787/session/my-session

# Send a message (JSON response)
curl -X POST http://127.0.0.1:8787/session/my-session/message \
  -H 'Content-Type: application/json' \
  -d '{"message": "hello"}'

# Resume in the TUI (session id from the list above)
jan-klod --addr 127.0.0.1:8787 <session-id>
```

The `--addr` makes this the REST path: *this* gateway, the one serving requests above, rather than a fresh one on a pipe. Without it, `jan-klod <session-id>` resumes over stdio — the store is the same either way.


## Next steps

- Add skills: create `.agents/skills/review.md` with a YAML `name:` header.
- Connect an MCP server: add a `registry.mcp.servers` entry in `config.yaml`.
- Read the [architecture overview](concepts/architecture.md) to understand the extension model.
