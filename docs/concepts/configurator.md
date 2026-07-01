---
type: concept
title: Configurator
description: Web UI for generating Jan-Klod configuration and deployment archives
tags: [configurator, ui, zip, setup]
created: 2026-06-28T00:00:00Z
updated: 2026-06-29T00:00:00Z
---

The Configurator is a Spring Initializr-style web UI. Users select extensions and provide settings; the UI generates a ready-to-run archive containing the core binary, selected `.wasm` extensions, and a pre-filled `jan-klod.yaml`.

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
  jan-klod.yaml         ← pre-filled from selections
  ext/
    provider-openai.wasm
    interceptor-intent-router.wasm
    interceptor-context.wasm
    store-sqlite.wasm
    api-rest.wasm
    …selected extensions…
  README.md
```

## Bundle presets

All bundles ship the same `jan-klod` **core** binary (built with Cargo) plus a
selected `.wasm` extension set. UI-oriented bundles additionally include the
separate **UI client** binary and enable an `api-rest` extension for it to
connect to. Presets differ only in which `.wasm` extensions are included, whether
the UI client is bundled, and what `jan-klod.yaml` is pre-filled with.

| Bundle | Included extensions | UI client |
|---|---|---|
| `tui` | providers + interceptors + stores + registries + tools + `api-rest` | included (launches in TUI mode) |
| `gui` | same | included (launches with `--gui`) |
| `full` | everything | included |

Core runs headless; the UI client selects its mode at launch: `jan-klod-ui`
(TUI), `jan-klod-ui --gui`, or a browser pointed at the `api-rest` surface.

## Extension registry

The registry is a simple HTTP file server: a directory of `.wasm` files with a metadata index. No crates.io, no npm. Extensions are downloaded at Configurator generation time and bundled into the ZIP.
