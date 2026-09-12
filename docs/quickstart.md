---
title: Quickstart
description: Install jan-klod, start the server, and run your first session in under 5 minutes.
---

# Quickstart

## 1. Install

**There is no release yet**, so the installer has nothing to download. It says so
rather than failing obscurely — `error: could not determine latest release tag`,
exit 1 — but that is still a dead end at step one, so build from a checkout
instead. The two routes are described below in that order: the one that works
today, then the one that will.

### Today: build from source

```sh
git clone https://github.com/PromptPasture/jan-klod
cd jan-klod
make setup                  # once: pinned cargo plugins, git hooks, WIT deps
make bundle DIST=coding
```

`make bundle` produces exactly what a release publishes:
`dist/jan-klod-<version>-<os>-<arch>-coding.tar.gz`, carrying `jan-klod`,
`jan-klod-gateway`, a pre-filled `config.yaml` and the staged `ext/`. Unpack it
wherever you keep tools and put the two binaries on your PATH. `DIST` takes
`coding`, `headless-chat` or `minimal`; add `GUI=1` for the desktop window.

To work in the checkout rather than install from it, see
[development setup](guides/development-setup.md).

### After the first release: the installer

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
```

That gives you **`coding`** — models, the interceptor set, and the file and git
tools. Two other distributions exist, and a distribution is about what the
install is *for* rather than which client it carries:

```sh
# a chat channel over Telegram; nothing that touches the machine
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh -s -- --dist headless-chat

# one provider and the interceptor set, and nothing else
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh -s -- --dist minimal
```

[What each one carries](concepts/configurator.md#distributions).

That installs `jan-klod` (TUI) and `jan-klod-gateway` (server) to `~/.local/bin`. Add it to your PATH if it isn't already:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

### Verifying what you downloaded

`install.sh` checks the bundle against `SHA256SUMS.txt` from the same release.
**That proves the download was not corrupted; it does not prove the bundle is
ours** — the checksums travel with the bundle, so whoever can serve you one can
serve you both and the pair will agree.

A minisign signature is what closes that, because the key is published where the
release cannot rewrite it. The installer does not check it, deliberately:
minisign is not on most machines, and an installer that verified only when the
tool happened to be present would report success for two quite different
situations. Verifying is one command:

```sh
minisign -Vm jan-klod-<version>-<os>-<arch>-coding.tar.gz -P <public key>
```

**No release has been signed yet**, because no release has been cut — see
[#93](https://github.com/PromptPasture/jan-klod/issues/93). The public key will
be on the [landing page](https://promptpasture.github.io/jan-klod/) and in this
repository when the first signed release exists; until then there is nothing to
verify against, and `registry.trusted-keys` in `config.yaml` ships empty, so
`ext install` refuses first-party components as untrusted rather than pretending
otherwise.

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

`jan-klod` starts its own `jan-klod-gateway rpc` and talks to it over that
process's stdin and stdout — no port, no token, and nothing left running when
you quit. The gateway uses the current directory as the workspace, so file tools
are jailed to it.

The single argument is the **session id**. To drive a gateway that is already
listening somewhere, name it with `--addr`:

```sh
jan-klod --addr 127.0.0.1:8787 my-session
```

That path uses the REST + SSE surface instead, and starts a gateway with
`serve --bind` if nothing answers there.

### The same client, in a window

```sh
jan-klod --gui
```

This does what `--addr` does — spawn or attach a gateway — and then opens the
core's own web client in a desktop window, using the system webview. There is no
bundled browser and no separate GUI codebase: it is the **same** page a browser
gets at the gateway's `/`, so anything true of one is true of the other. The
token comes across with it, so nothing asks you to retype it.

`--gui` takes `--addr` like the other modes and takes **no session id** — the
page chooses its own, and it refuses one rather than ignoring it.

The window ships only in the `-gui` archive (`install.sh --gui`, or
`make bundle GUI=1` from a checkout). Without it, `--gui` says the shell is not
installed and exits non-zero instead of quietly starting the terminal UI. On
Linux the webview is a system package; if the window will not open, install
`libwebkit2gtk-4.1-0` and `libayatana-appindicator3-1`. On macOS it is part of
the OS.

To run the gateway yourself — as a background service, or to share one between
clients:

```sh
jan-klod-gateway serve --bind 127.0.0.1:8787
```

Or to speak the client protocol on a pipe, which is what `jan-klod` does for you
and what an editor would do:

```sh
jan-klod-gateway rpc
```

It reads newline-delimited JSON-RPC 2.0 on stdin and writes it on stdout;
**stdout carries nothing else**, so logs and errors go to stderr.

## Point another agent at it (MCP)

Claude Code, Codex, Goose and editors consume MCP servers. `jan-klod` is one:

```sh
jan-klod-gateway mcp
```

Same pipe discipline as `rpc` — the client spawns it, frames on stdout, logs on
stderr. Three tools: `ask` (one turn, returning the answer), `session_list` and
`session_get`. Point a client at it the way you point at any stdio MCP server;
for Claude Code that is:

```sh
claude mcp add jan-klod -- jan-klod-gateway mcp
```

Run it from the repository you want it to work on — the gateway uses the current
directory as the workspace, so file tools are jailed to it.

**A turn over MCP will not write or run commands.** There is nobody to answer a
confirmation, so each one takes its default, which is a refusal — the same rule
as a scripted `ask`. Reads and searches work; a write comes back as a refusal
the calling model can see and explain. If you want writes, drive it from `jan-klod`
or the REST surface, where someone can say yes.

Naming `config.yaml` and `ext` explicitly also works, but is only right inside a
checkout: without them the gateway uses the current directory when it holds them
and the installed copies otherwise, which is what makes `cd my-repo && jan-klod`
work.

## One question, no server

For a single answer — in a shell, or a CI step — there is nothing to leave
running:

```shell
cd my-repo
jan-klod-gateway ask "what does this repo do?"
```

The answer goes to stdout and nothing else does, so it pipes. Confirmations are
asked on the terminal, since there is one; with stdin closed (a pipe, a CI job)
each takes the prompt's default, which is a refusal — so a scripted `ask` reads
and searches but will not write or run commands unless someone is there to say
yes.

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
jan-klod-gateway verify --live
```

It resolves every extension your config enables, reports anything absent from
`ext/`, then starts them all — exiting non-zero if any part of the install is
incomplete. The release bundles are built through the same check.

`--live` then asks the model one question. That part matters: everything above it
is offline, and passes with a wrong API key, a local server that is not running, a
model name that does not exist, or a `base-url` egress refuses — which is the whole
list of things that go wrong on a first run. Drop `--live` in a build step, where
spending a request would be rude; keep it when you are asking "why doesn't this
work?".

To see what is in `ext/` and what each component may ask the host for — which
`ls` cannot tell you:

```sh
jan-klod-gateway ext list
```

A component from somewhere other than this repository goes in with `ext install`,
which verifies before it copies rather than after — from a path or a URL:

```sh
jan-klod-gateway ext install ./tool-thing.wasm --sha256 <hex> --allow-unsigned
jan-klod-gateway ext install https://example.com/ext/tool-thing.wasm
```

A URL names the component; the manifest and signatures are fetched from beside
it. Only public destinations are allowed, checked before anything is fetched and
again on every redirect.

Signed is the default; `--allow-unsigned` is the deliberate way past it and
requires `--sha256`, because waiving the signature leaves the digest as the only
evidence about those bytes. Name the keys you trust in `registry.trusted-keys`
and the flags are unnecessary — see
[Configuration → Putting something in `ext/`](concepts/configuration.md#putting-something-in-ext-registry).
A refused install leaves `ext/` exactly as it was.

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
jan-klod --addr 127.0.0.1:8787 <session-id>
```

The `--addr` is what makes this the REST path: it says *this* gateway, the one
serving the requests above, rather than a fresh one on a pipe. Without it,
`jan-klod <session-id>` resumes the same session over stdio — the store is the
same either way.


## Next steps

- Add skills: create `.agents/skills/review.md` with a YAML `name:` header.
- Connect an MCP server: add a `registry.mcp.servers` entry in `config.yaml`.
- Read the [architecture overview](concepts/architecture.md) to understand the extension model.
