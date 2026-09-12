---
type: concept
title: Configurator
description: Web UI for generating Jan-Klod configuration and deployment archives
tags: [configurator, ui, zip, setup]
created: 2026-06-28T00:00:00Z
updated: 2026-09-08T00:00:00Z
---

The Configurator is a Spring Initializr-style web UI. Users select extensions and provide settings; the UI generates a ready-to-run archive containing the core binary, selected `.wasm` extensions, and a pre-filled `config.yaml`.

Hosted publicly. Launch on **GitHub Pages**; migrate to `start.janklod.dev` once the domain is set up. Self-hosted mode always supported.

## UI flow

1. Pick a bundle preset (TUI, GUI, full) or start from scratch.
2. Select extensions from the registry (search, filter by role category).
3. Fill in configuration values (LLM endpoint, API keys, storage backend, etc.).
4. Click **Generate** — the server resolves the dependency graph, validates WIT interface compatibility, and builds the ZIP.

## ZIP layout — standard

```
jan-klod-<version>-<os>-<arch>/
  jan-klod              ← Rust core binary (platform-specific)
  jan-klod-ui           ← UI client binary (UI bundles only; TUI/GUI by launch flag)
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

A distribution says what an install is **for**. It is not a client choice —
whether you look at the runtime through a terminal, a window or a browser is
decided at launch and is orthogonal to what the runtime carries. Naming bundles
`tui`/`gui`/`full`, as this page used to, mixed those two questions under one
word.

Every distribution ships the same `jan-klod` **core** binary plus a selected
`.wasm` extension set. The core's **REST surface is built in** (host-side,
`jan-klod-gateway serve`), so there is no `api-rest` guest to include; a UI
client is a separate binary that connects to it.

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

Each distribution is a directory under `scripts/distributions/<name>/`: a
`guests` list and a `config.yaml`. `make bundle DIST=<name>` builds an archive
from the pair, and the release publishes all three per platform.

Three places name them — the definitions, `scripts/install.sh`, and
`.github/workflows/release.yml` — and none can see the others, so
`docs_match_config::the_installer_offers_the_distributions_the_release_builds`
holds them to one set. A disagreement between any two would be a 404 at whoever
ran the installer, and it would not surface until after a release.

**Adding a fourth is adding a directory.** Nothing here enumerates them: the
tests read `scripts/distributions/` from disk, so a new one is covered the day
it lands rather than the day somebody remembers this page exists.

### When a Configurator UI arrives

There is no interactive Configurator yet — the landing page is static. When one
is built it reads these same definitions rather than carrying its own list, and
the client choice (TUI, GUI, browser) is a separate toggle beside the
distribution picker, because it is a separate question. That is why this section
describes distributions as data and not as a menu: the menu is a view of the
data, and the data is already here.

## Extension registry

The registry is a simple HTTP file server: a directory of `.wasm` files with a metadata index. No crates.io, no npm. Extensions are downloaded at Configurator generation time and bundled into the ZIP.

**Planned, Phase 16.** The index gains, per extension, its `api-version`, the
capabilities its manifest requests, a checksum and a signature; the Configurator
shows requested capabilities *before* download, and the gateway's `ext install` subcommand
verifies provenance before anything reaches `ext/`. See the
[roadmap](roadmap.md#phase-16--capability-manifest--signed-registry).
