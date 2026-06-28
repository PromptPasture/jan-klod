---
type: concept
title: Architecture
description: High-level architecture of the Jan-Klod agent runtime
tags: [architecture, core, extensions, go, wasm, wazero]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T00:00:00Z
---

## Philosophy

Linux kernel model: the **core** is a minimal container with no domain logic. All agent behaviour is provided by **extensions** loaded at runtime.

## Core responsibilities

- Extension lifecycle management (load, enable, disable, unload)
- Configuration loading (`jan-klod.yaml`)
- WASM component host (Wazero)
- HTTP API (REST + SSE for streaming)
- Event bus (extension-to-extension communication)
- Observability (structured logging, Prometheus metrics, OpenTelemetry traces)

Zero agent behaviour in core. A core-only boot starts up and does nothing.

## Extension model

Extensions are **WASM components** (`.wasm` files) dropped into `ext/`. They are loaded at runtime by Wazero and sandboxed — they can only do what the WIT interface explicitly grants.

### What the host grants extensions

- Outbound HTTP (to call LLM APIs, web search, etc.)
- Storage read/write via the `MemoryStore` WIT interface
- Logging
- Config read (own section only)
- Event bus publish/subscribe

### What extensions cannot do

- Direct filesystem access
- Open arbitrary network connections
- Talk to other extensions directly (routed through core)
- Access host OS or process

### Extension taxonomy

| Category | Class | Mechanism | Examples |
|---|---|---|---|
| `provider-*` | LLM API clients | WASM | `provider-openai`, `provider-ollama`, `provider-anthropic` |
| `manager-*` | Stateful orchestrators | WASM | `manager-agent-loop`, `manager-context` |
| `store-*` | Persistence backends | WASM | `store-sqlite`, `store-postgres`, `store-supabase` |
| `registry-*` | Capability catalogues | WASM | `registry-skills`, `registry-mcp` |
| `tool-*` | Discrete callable tools | WASM | `tool-web-search` |
| `api-*` | Network API surfaces | Native Go | `api-rest`, `api-grpc`, `api-graphql` |
| `ui-*` | User interfaces | Native Go | `ui-tui`, `ui-web`, `ui-gui` |
| `chat-*` | Chat platform integrations | Native Go | `chat-slack`, `chat-telegram`, `chat-whatsapp`, `chat-mattermost` |

WASM extensions are sandboxed and language-agnostic. Native Go extensions are compiled into the binary and have OS access (ports, terminal, window system, long-lived connections).

### Extension dependency graph

```
provider-anthropic  ─┐
provider-openai     ─┤ implements llm-provider WIT interface
provider-ollama     ─┘

manager-context       implements context-manager WIT interface

manager-agent-loop    requires:  llm-provider, context-manager
                      optional:  memory-store, skill-registry, mcp-registry
                      implements agent-manager WIT interface

store-sqlite          implements memory-store WIT interface (default)
store-postgres        implements memory-store WIT interface
store-supabase        implements memory-store WIT interface

registry-skills       implements skill-registry WIT interface
registry-mcp          implements mcp-registry WIT interface

tool-web-search       requires: agent-manager

api-rest              requires: agent-manager (exposes it over HTTP + SSE)
api-grpc              requires: agent-manager
api-graphql           requires: agent-manager

ui-tui                requires: agent-manager (via internal Go interface)
ui-web                requires: api-rest
ui-gui                requires: agent-manager (via internal Go interface)

chat-slack            requires: agent-manager (via internal Go interface)
chat-telegram         requires: agent-manager (via internal Go interface)
chat-whatsapp         requires: agent-manager (via internal Go interface)
```

### UI extensions (native)

UI components need OS-level access (terminal, window system) and cannot run inside the WASM sandbox. They are **native Go extensions** — compiled into the binary, but following the same extension conventions (naming, config, lifecycle) as WASM extensions. They implement the `UIProvider` native Go interface rather than a WIT interface.

| Extension | Mode | Technology |
|---|---|---|
| `ui-tui` | `jan-klod` (default) | Bubble Tea |
| `ui-web` | `jan-klod --web` | Embedded HTTP server, opens browser tab |
| `ui-gui` | `jan-klod --gui` | Wails native WebView window (CGo) |

Configured in `jan-klod.yaml` like any other extension:

```yaml
extensions:
  ui-tui: true
  ui-web:
    port: 8080
  ui-gui: false
```

Only one UI extension is active at a time, selected by flag. All modes speak the same internal REST API and load the same `.wasm` extensions.

## Agent loop architecture

```
User query
    │
    ▼
Intent router ──→ direct answer (no agent)
    │
    ▼
Step controller (selects tools, compresses history, builds prompt)
    │
    ▼
LLM (constrained decoding)
    │
    ▼
Parse & validate action → retry on failure
    │
    ▼
Tool execution → loop back to step controller
    │
    ▼
Answer extractor
```

See [Small-Model Harness](small-model-harness.md) for mitigation strategies.

## Transport

REST via Go `net/http` + `chi` router. Streaming agent responses use **Server-Sent Events (SSE)**. Curl-debuggable, browser-compatible, no stub generation.

## Storage

| Extension | Backend | Notes |
|---|---|---|
| `store-sqlite.wasm` | SQLite (`modernc/sqlite`) | Default — pure Go, no CGo, zero-ops |
| `store-postgres.wasm` | PostgreSQL (`pgx`) | Self-hosted, multi-user |
| `store-supabase.wasm` | Supabase | Hosted Postgres + realtime + auth |

SQL layer: **sqlc** — type-safe Go generated from `.sql` files.

Only one `memory-store` is active at a time; selected via `jan-klod.yaml`.

## Stack

| Layer | Technology |
|---|---|
| Core language | Go |
| WASM host | Wazero (pure Go, no CGo) |
| Extension format | WASM component model + WIT interfaces |
| HTTP | `chi` + `net/http` (REST + SSE) |
| SQL | `sqlc` + `modernc/sqlite` (default) |
| UI (native Go) | `ui-tui` (Bubble Tea), `ui-web` (HTTP server), `ui-gui` (Wails WebView) |
| Build | Wails (CGo required for `ui-gui`; pure Go sufficient without it) |
| Linting | `golangci-lint` |
| Observability | Structured logging + Prometheus + OpenTelemetry |
| Config | YAML (`jan-klod.yaml`) |

## Testing

- **Unit:** standard Go `testing` package
- **Integration:** Go test with real SQLite + embedded Wazero
- **Extension:** WASM component loaded in test harness, WIT interface verified

## Deployment modes

- **Standard:** single Go binary + `ext/*.wasm` + `jan-klod.yaml`
- **Bundle:** pre-packaged ZIP with binary + curated `.wasm` set + pre-filled config

See [Blue/Green Deployment](blue-green-deployment.md) for the update strategy.
