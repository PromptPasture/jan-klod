---
title: jan-klod — sandboxed coding agent
---

# jan-klod

A sandboxed coding agent powered by the WebAssembly Component Model.

![Rust](https://img.shields.io/badge/Rust-orange?style=flat-square)
![WebAssembly](https://img.shields.io/badge/WebAssembly-654FF0?style=flat-square)
![Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square)

## What it is

jan-klod is a coding agent that runs each extension (provider, tool, interceptor) as a sandboxed WebAssembly component. Extensions cannot access the filesystem, network, or environment unless explicitly granted — without requiring a container.

## Why it's different

Most agents run extensions in the same process with full system access. jan-klod uses the **WebAssembly Component Model**: typed WIT contracts define exactly what each extension can import, and the host enforces fail-closed permission gates. You get Anthropic Claude, file tools, skill shortcuts, and MCP server integration — all sandboxed.

## Install

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
```

That installs **coding**, the default. A distribution says what the install is
*for* — not which client it carries, since terminal, window or browser is
chosen at launch.

| Distribution | What it carries | For |
|---|---|---|
| **coding** | models, the interceptor set, file and git tools, skills | working on a codebase |
| **headless-chat** | models and the interceptor set, nothing that touches the machine | a chat channel over Telegram — a Pi or a container |
| **minimal** | one provider and the interceptor set | the smallest thing that still runs a turn |

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh -s -- --dist headless-chat
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh -s -- --dist minimal
```

## Quick start

```sh
export OPENAI_API_KEY=sk-...
jan-klod my-session
```

`jan-klod` starts the gateway automatically if it isn't already running.

See the [quickstart guide](https://github.com/PromptPasture/jan-klod/blob/main/docs/quickstart.md) for a full walkthrough.

## Links

- [GitHub repository](https://github.com/PromptPasture/jan-klod)
- [Releases](https://github.com/PromptPasture/jan-klod/releases)
- [Quickstart](https://github.com/PromptPasture/jan-klod/blob/main/docs/quickstart.md)
- [Roadmap](https://github.com/PromptPasture/jan-klod/blob/main/docs/concepts/roadmap.md)

---

jan-klod is open source under the Apache-2.0 license.
