---
title: jan-klod — sandboxed coding agent
---

# jan-klod

A sandboxed coding agent powered by the WebAssembly Component Model.

![Rust](https://img.shields.io/badge/Rust-orange?style=flat-square)
![WebAssembly](https://img.shields.io/badge/WebAssembly-654FF0?style=flat-square)
![Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue?style=flat-square)

## What it is

Runs each extension as a sandboxed WebAssembly component. No filesystem, network, or environment access unless granted—no container needed.

## Why it's different

Most agents run extensions in-process with full access. jan-klod isolates via **WebAssembly Component Model**: WIT contracts limit imports, hosts enforce fail-closed gates. Claude, file tools, skills, MCP—all sandboxed.

## Install

**No release yet.** Build from checkout until first tag:

```sh
git clone https://github.com/PromptPasture/jan-klod
cd jan-klod
make setup                  # once: pinned cargo plugins, git hooks, WIT deps
make bundle DIST=coding
```

Creates release-like archive in `dist/`. Unpack; add `jan-klod` and `jan-klod-gateway` to PATH.

Below is the release-time installer (now has nothing to fetch):

```sh
curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh
```

Installs **coding** by default. Distribution is purpose, not client (terminal/window/browser chosen at launch).

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

`jan-klod` starts gateway auto if needed.

See the [quickstart guide](https://github.com/PromptPasture/jan-klod/blob/main/docs/quickstart.md) for a full walkthrough.

## Verifying a download

Installer checks SHA-256 (proves no corruption, **not** authenticity). Minisign signature proves ownership; its public key goes here—key nobody finds is signature nobody checks.

**Not signed yet** (no release). `registry.trusted-keys` ships empty; `ext install` refuses untrusted first-party components. At first signed release, key appears here; verify via:

```sh
minisign -Vm <bundle> -P <the key above>
```

## Links

- [GitHub repository](https://github.com/PromptPasture/jan-klod)
- [Releases](https://github.com/PromptPasture/jan-klod/releases)
- [Quickstart](https://github.com/PromptPasture/jan-klod/blob/main/docs/quickstart.md)
- [Roadmap](https://github.com/PromptPasture/jan-klod/blob/main/docs/concepts/roadmap.md)

---

jan-klod is open source under the Apache-2.0 license.
