---
type: concept
title: Configurator
description: Web UI for generating Jan-Klod configuration and deployment archives
tags: [configurator, ui, zip, setup]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T00:00:00Z
---

The Configurator is a Spring Initializr-style web UI. Users select extensions and provide settings; the UI generates a ready-to-run archive containing the core binary, selected `.wasm` extensions, and a pre-filled `jan-klod.yaml`.

Aspirational public hosting: `start.janklod.dev`. Self-hosted mode is always supported.

## UI flow

1. Pick a bundle preset (TUI, GUI, full) or start from scratch.
2. Select extensions from the registry (search, filter by role category).
3. Fill in configuration values (LLM endpoint, API keys, storage backend, etc.).
4. Click **Generate** — the server resolves the dependency graph, validates WIT interface compatibility, and builds the ZIP.

## ZIP layout — standard

```
jan-klod-<version>-<os>-<arch>/
  jan-klod              ← Go binary (platform-specific)
  jan-klod.yaml         ← pre-filled from selections
  ext/
    provider-openai.wasm
    manager-agent-loop.wasm
    store-sqlite.wasm
    …selected extensions…
  README.md
```

## Bundle presets

All bundles ship the same `jan-klod` binary (built with Wails). Presets differ only in which `.wasm` extensions are included and what `jan-klod.yaml` is pre-filled with.

| Bundle | Included extensions | Default UI mode |
|---|---|---|
| `tui` | all providers + managers + stores + registries + tools | TUI |
| `gui` | same | GUI (`--gui`) |
| `full` | everything | TUI |

Users can always switch mode at runtime: `jan-klod`, `jan-klod --web`, `jan-klod --gui`.

## Extension registry

The registry is a simple HTTP file server: a directory of `.wasm` files with a metadata index. No Maven Central, no npm. Extensions are downloaded at Configurator generation time and bundled into the ZIP.
