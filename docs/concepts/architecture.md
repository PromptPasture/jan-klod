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

**Design rules (standing):**
- **KISS** — every extension does one thing. If it grows a second responsibility, split it.
- **YAGNI** — do not add capability until there is a concrete use case. Pluggability is not an excuse to over-engineer.

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
| `agent-*` | AI agent delegation via ACP | WASM | `agent-claude-code`, `agent-opencode`, `agent-codex` |
| `api-*` | Network API surfaces | Native Go | `api-rest`, `api-grpc`, `api-graphql` |
| `ui-*` | User interfaces | Native Go | `ui-tui`, `ui-web`, `ui-gui` |
| `chat-*` | Chat platform integrations | Native Go | `chat-slack`, `chat-telegram`, `chat-whatsapp`, `chat-mattermost` |

WASM extensions are sandboxed and language-agnostic. Native Go extensions are compiled into the binary and have OS access (ports, terminal, window system, long-lived connections).

Jan-Klod speaks ACP both ways — as a client (`agent-*` extensions call other agents) and as a server (it can be called by other ACP orchestrators).

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

agent-claude-code     requires: agent-manager (delegates tasks via ACP)
agent-opencode        requires: agent-manager

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

## Provider fallback

When a provider or model fails (unavailable, rate-limited, quota exceeded, local OOM), `manager-agent-loop` falls back through a two-level priority list defined in `jan-klod.yaml`:

```yaml
providers:
  - provider: provider-anthropic
    models:
      - claude-sonnet-4-6
      - claude-haiku-4-5        # cheaper fallback within same provider
  - provider: provider-openai
    models:
      - gpt-4o
      - gpt-4o-mini
  - provider: provider-ollama   # local, always available
    models:
      - qwen2.5:14b
      - qwen2.5:7b              # smaller if 14b OOM
```

Fallback order: try each model within the current provider → move to next provider → if all exhausted, surface error to user (no silent retry spiral).

Fallback is per-request — if the primary recovers, the next request uses it again. This also enables cost routing: cheap tasks naturally route to smaller/cheaper models without a separate configuration.

## Task routing

`manager-agent-loop` classifies each request into a task type and routes it to the configured provider/model. Jan-Klod ships built-in task types as sensible defaults; users extend or override in `jan-klod.yaml`.

**Built-in task types:**

| Task type | Default use |
|---|---|
| `code-generation` | Generate new code |
| `code-review` | Review existing code |
| `file-edit` | Edit one or more files |
| `web-search` | Search and retrieve web content |
| `research` | Multi-step information gathering |
| `reasoning` | Complex analysis or planning |
| `planning` | Break down goals into steps |
| `chat` | General conversation |
| `clarification` | Resolve ambiguity |
| `agent-delegation` | Delegate to another AI agent via ACP |

User-defined types can be added to `jan-klod.yaml` — the LLM classifier receives the full list at runtime and picks the closest match. No code changes needed to add a type.

```yaml
routing:
  code-generation:   provider-ollama/qwen2.5:14b
  code-review:       provider-ollama/qwen2.5:14b
  file-edit:         provider-ollama/qwen2.5:14b
  reasoning:         provider-anthropic/claude-sonnet-4-6
  planning:          provider-anthropic/claude-sonnet-4-6
  web-search:        provider-openai/gpt-4o-mini
  chat:              provider-anthropic/claude-haiku-4-5
  clarification:     provider-anthropic/claude-haiku-4-5
  agent-delegation:  provider-anthropic/claude-sonnet-4-6
  # user-defined:
  data-analysis:     provider-openai/gpt-4o
```

## Parallel decomposition

For tasks where subtasks are independent, `manager-agent-loop` decomposes and dispatches in parallel:

- **`file-edit`** — each file edited in parallel, results merged
- **`web-search`** — multiple queries in parallel, results merged before LLM synthesis
- **`research`** — multiple sources fetched in parallel
- **`code-review`** — each module reviewed independently

Single-subtask requests skip decomposition and route directly. The decomposer and merger live inside `manager-agent-loop` — no new extension needed.

```
Task
  │
  ▼
Decomposer ──→ single subtask? route directly
  │
  ▼ multiple independent subtasks
Parallel dispatch → [model A]  [model B]  [model C]
  │
  ▼
Merger (assembles results into coherent context)
  │
  ▼
Final LLM call (synthesis / answer)
```

## MCP fault tolerance

`registry-mcp` monitors connected MCP servers. On crash or disconnect:

1. Mark the server as `down`.
2. Remove its tools from the active tool set — `manager-agent-loop` will not offer them.
3. Emit an event on the bus — `ui-*` extensions display a warning to the user.
4. Attempt reconnect on an exponential backoff timer.

No crash propagates to core. The agent loop continues with the remaining tools.

## Config hot-reload

Extensions can pick up `jan-klod.yaml` changes without restart. Core watches the config file and notifies affected extensions via the event bus. Extensions opt in to hot-reload by implementing the reload lifecycle hook.

## Deployment targets

| Target | Notes |
|---|---|
| Desktop (macOS, Windows, Linux) | Primary target; all UI modes available |
| ARM home server / NAS | Low memory footprint (Go + WASM); headless, `api-rest` + `chat-*` extensions |
| Docker | Single container; config via environment variables or mounted `jan-klod.yaml` |
| Kubernetes | Enterprise; horizontal scaling of stateless API layer; shared `store-postgres` or `store-supabase` |

## Deployment modes

- **Standard:** single Go binary + `ext/*.wasm` + `jan-klod.yaml`
- **Bundle:** pre-packaged ZIP with binary + curated `.wasm` set + pre-filled config

See [Blue/Green Deployment](blue-green-deployment.md) for the update strategy.
