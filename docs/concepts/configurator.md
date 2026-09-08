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

## Bundle presets

All bundles ship the same `jan-klod` **core** binary (built with Cargo) plus a
selected `.wasm` extension set. The core's **REST surface is built in** (host-side,
`jan-klod serve`), so UI-oriented bundles simply add the separate **UI client**
binary that connects to it — no `api-rest` guest to include. Presets differ only in
which `.wasm` extensions are included, whether the UI client is bundled, and what
`config.yaml` is pre-filled with.

| Bundle | Included guests | UI client |
|---|---|---|
| `tui` | providers + interceptors + registries + tools | included (launches in TUI mode) |
| `gui` | same | included (launches with `--gui`) |
| `full` | everything | included |

Core runs headless and serves REST itself; the UI client selects its mode at
launch: `jan-klod-ui` (TUI), `jan-klod-ui --gui`, or a browser pointed at the core's
REST surface.

**Planned — distributions.** The [vision](../decisions/2026-09-08-harness-platform-vision/Vision.md#product-shape--kernel-distributions-clients)
names bundles by *what they are for*, not by which client they carry — the Linux
distribution analogy. The presets become `coding` (providers + interceptors +
the file/edit/find/git tool fleet + skills; UI client included), `headless-chat`
(providers + interceptors + a chat channel; no UI client — Raspberry Pi and
container), and `minimal` (one provider, the interceptor set, no tools). Client
choice (TUI, GUI, web) is orthogonal and made at launch. Tracked as a
cross-cutting issue, not a phase.

## Extension registry

The registry is a simple HTTP file server: a directory of `.wasm` files with a metadata index. No crates.io, no npm. Extensions are downloaded at Configurator generation time and bundled into the ZIP.

**Planned, Phase 16.** The index gains, per extension, its `api-version`, the
capabilities its manifest requests, a checksum and a signature; the Configurator
shows requested capabilities *before* download, and the gateway's `ext install` subcommand
verifies provenance before anything reaches `ext/`. See the
[roadmap](roadmap.md#phase-16--capability-manifest--signed-registry).
