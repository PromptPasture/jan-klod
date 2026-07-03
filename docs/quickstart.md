---
title: Quickstart
description: Install jan-klod, start the server, and run your first session in under 5 minutes.
---

# Quickstart

## 1. Install

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
```

This installs the `jan-klod` binary to `~/.local/bin`. Add it to your PATH if it isn't already:

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

## 3. Start the server

From inside a repository you want to work on:

```sh
cd ~/my-project
jan-klod serve config.yaml ext
```

The server listens on `127.0.0.1:8787` by default. jan-klod automatically uses
the current directory as the workspace — file tools are jailed to it.

## 4. Connect the TUI

In another terminal:

```sh
jan-klod-ui 127.0.0.1:8787 my-session
```

Type a message and press **Enter** to send it. The model's response streams in token by token. Press **Esc** to quit.

## 5. Ask it to do something

```
> Read src/main.rs and summarize what it does
> Add a --verbose flag to the CLI
> Run the tests and fix any failures
```

File reads, writes, and shell commands go through permission-gated sandboxed extensions.
By default, only file access is enabled. To enable shell execution, set `extensions.interceptor.permission.enabled: true` in `config.yaml`.

## 6. Resume a session

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
jan-klod-ui 127.0.0.1:8787 <session-id>
```

## Next steps

- Add skills: create `.agents/skills/review.md` with a YAML `name:` header.
- Connect an MCP server: add a `registry.mcp.servers` entry in `config.yaml`.
- Read the [architecture overview](concepts/architecture.md) to understand the extension model.
