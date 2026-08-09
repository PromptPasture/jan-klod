---
title: Quickstart
description: Install jan-klod, start the server, and run your first session in under 5 minutes.
---

# Quickstart

## 1. Install

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
```

This installs `jan-klod` (TUI) and `jan-klod-gateway` (server) to `~/.local/bin`. Add it to your PATH if it isn't already:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

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

`jan-klod` automatically starts `jan-klod-gateway` in the background if it isn't
already running. The gateway listens on `127.0.0.1:8787` and uses the current
directory as the workspace — file tools are jailed to it.

To start the gateway manually (e.g. as a background service):

```sh
jan-klod-gateway serve --bind 127.0.0.1:8787
```

Naming `config.yaml` and `ext` explicitly also works, but is only right inside a
checkout: without them the gateway uses the current directory when it holds them
and the installed copies otherwise, which is what makes `cd my-repo && jan-klod`
work.

Type a message and press **Enter** to send it. The model's response streams in token by token. Press **Esc** to quit.

## 4. Ask it to do something

```
> Read src/main.rs and summarize what it does
> Add a --verbose flag to the CLI
> Run the tests and fix any failures
```

File reads, writes, and shell commands go through permission-gated sandboxed extensions.
Enabled by default: `tool.fs` (read / write / grep — `grep` searches the whole
workspace tree by default), `tool.edit` (partial edits), and `tool.find` (glob the
workspace, e.g. `**/*.rs`).
Command execution is off entirely: set `execution.enabled: true` in `config.yaml` to
turn on the substrate, then enable the tool you want on top of it — `tool.git`
(read-only `status`/`diff`/`log`/`show`/`branch`, which cannot modify the repository)
or `tool.shell` (any command, from the model). Every write and command is confirmed
with you first by the `permission` interceptor, which is on by default; turning it off
removes that prompt.

The confirmation appears in the TUI as a question with the answers it accepts; the
next Enter answers the waiting turn instead of sending a new message (an empty line
takes the safe default). You can answer `yes`/`no` for that one call, or `always`/`never` to
decide for that whole kind of action — `fs:write`, `shell:cargo` — for the rest of the
run. Those standing decisions are held in memory only: restart and it asks again. They
also never cover an argument pointing outside the workspace, so "always allow writes"
does not become permission to write `/etc/passwd`.

`tool.edit` changes part of a file instead of rewriting it. It first shows the model
each line with an **anchor** (a hash of the line's position and text), and edits name
those anchors. If the file changed since the model looked, the anchors no longer match
and the edit is refused rather than applied to the wrong lines.

## 5. Reach it from another machine

The gateway binds `127.0.0.1` by default and, unset, has no authentication —
which is fine for a loopback socket and not fine anywhere else. To expose it:

```sh
export JAN_KLOD_TOKEN=$(openssl rand -hex 32)
jan-klod-gateway serve --bind 0.0.0.0:8787
```

Every route except `/health` then requires `Authorization: Bearer <token>`. Set
the same variable where you run `jan-klod` and the client sends it for you. There
is no TLS: put it behind something that terminates it if the network is not
trusted.

## 6. Check the install

If a tool seems missing, ask the runtime what it actually loaded:

```sh
jan-klod-gateway verify config.yaml ext
```

It resolves every extension your config enables, reports anything absent from
`ext/`, then starts them all — exiting non-zero if any part of the install is
incomplete. The release bundles are built through the same check.

## 7. Resume a session

Sessions persist across server restarts (SQLite store). The REST API:

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
jan-klod 127.0.0.1:8787 <session-id>
```

## Next steps

- Add skills: create `.agents/skills/review.md` with a YAML `name:` header.
- Connect an MCP server: add a `registry.mcp.servers` entry in `config.yaml`.
- Read the [architecture overview](concepts/architecture.md) to understand the extension model.
