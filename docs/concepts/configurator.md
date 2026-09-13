---
type: concept
title: Configurator
description: Web UI for generating Jan-Klod configuration and deployment archives
tags: [configurator, ui, zip, setup]
created: 2026-06-28T00:00:00Z
updated: 2026-09-08T00:00:00Z
---

The Configurator is a Spring Initializr-style web UI: select extensions, provide settings, generate a ready-to-run archive with core, `.wasm` extensions, and `config.yaml`.

Hosted on **GitHub Pages**; self-hosted mode always supported.

## UI flow

1. Pick a **distribution** (the purpose: coding, headless-chat, minimal) or start from scratch.
2. Select extensions from the registry (search, filter by role).
3. Fill configuration values (LLM endpoint, API keys, storage backend).
4. Click **Generate** — resolves dependency graph, validates compatibility, builds ZIP.

## ZIP layout — standard

```
jan-klod-<version>-<os>-<arch>[-<distribution>][-gui]/
  jan-klod-gateway    ← the core binary (platform-specific)
  jan-klod            ← the client: line REPL, or `tui`, or `--gui` by launch mode
  jan-klod-gui        ← the Tauri window — `-gui` archives only (make bundle GUI=1)
  config.yaml         ← pre-filled from selections
  ext/                ← provider/interceptor/tool guests (persistence, the REST
                        surface, telegram, and delegation are host-side in the core
                        binary as of Phase 3/4 — not guests)
    provider-openai.wasm
    interceptor-intent-router.wasm
    interceptor-context.wasm
    …selected extensions…
  README.md
```

## Distributions

A distribution says what an install is **for**. It is not a client choice — whether you view the runtime through terminal, window, or browser is decided at launch and is orthogonal to what the runtime carries.

Every distribution ships the same `jan-klod` **core** binary plus a selected `.wasm` extension set. The core's **REST surface is built in** (host-side), so no `api-rest` guest is included; UI clients are separate binaries that connect to it.

| Distribution | What it carries | For |
|---|---|---|
| `coding` | providers, the interceptor set, `tool-fs`/`tool-edit`/`tool-find`/`tool-git`, skills | working on a codebase; the default |
| `headless-chat` | providers and the interceptor set, nothing that touches the machine | a chat channel over Telegram — a Raspberry Pi or a container |
| `minimal` | one provider and the interceptor set | the smallest thing that still runs a turn |

Install one with the installer's `--dist`:

```console
$ curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh -s -- --dist headless-chat
```

With no `--dist` you get `coding`.

### They are data, not code

Each distribution is a directory under `scripts/distributions/<name>/`: a `guests` list and `config.yaml`. `make bundle DIST=<name>` builds an archive from the pair; releases publish all three per platform.

Three places name them — definitions, `scripts/install.sh`, and `.github/workflows/release.yml` — and none see the others, so a test holds them in sync. A disagreement surfaces as a 404 after release.

**Adding a fourth is adding a directory.** Tests read `scripts/distributions/` from disk, so a new one is covered the day it lands.

### When a Configurator UI arrives

No interactive Configurator yet — the landing page is static. When built, it reads the same definitions rather than carrying its own list. The client choice (TUI, GUI, browser) is a separate toggle beside the distribution picker because it is a separate question. Distributions are data, not a menu: the menu is a view of the data.

## Extension registry

The registry is a simple HTTP file server: a directory of `.wasm` files with a metadata index. No crates.io, no npm. When a Configurator exists, extensions download at generation time and bundle into the ZIP. Built today: the index and `ext` commands that read it.

### What is built

The index is `index.json`, generated from staged components by `make registry-index` and published by a release. Per extension: name, version, `api-version`, kind, **declared capabilities**, description, publisher, URL, SHA-256, signature, and size. Digest and signature describe the individual `.wasm` and `.minisig`, not an archive, because `ext install` verifies per file.

Capabilities are not hand-typed: `scripts/manifests.sh` reads them from the component's real imports; the generator reads the manifest through the same code the host uses at boot. The index line is what the host enforces.

`registry.url` in `config.yaml` names the index. An `http`/`https` URL is fetched under the egress policy — public destinations only, every redirect re-checked. Anything else is a path, so a mirrored index works offline.

**`ext search` is the capability view.** No interactive Configurator yet (landing page is static), so the command is where an operator reads what a component asks for before downloading:

```console
$ jan-klod-gateway ext search tool-fs
tool-fs  0.1.0  api 0.3.0  tool  147445 bytes  unsigned
  may use host-fs
  fs tool: read / write / grep a workspace file through host-fs, dispatched by an `op` argument.
```

`ext list --remote` shows everything unfiltered; `ext install <name>` resolves through the index and hands the entry's URL and digest to the same verified install a path or URL gets. Reading an index grants nothing: `registry.trusted-keys` decides what lands in `ext/`, so with that list empty every resolved install is refused as untrusted — correct until something first-party is signed. See [security model](security-model.md) and [roadmap](roadmap.md#phase-16--capability-manifest--signed-registry).
