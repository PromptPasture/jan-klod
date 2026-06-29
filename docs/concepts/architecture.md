---
type: concept
title: Architecture
description: High-level architecture of the Jan-Klod agent runtime
tags: [architecture, core, extensions, rust, wasm, wasmtime]
created: 2026-06-28T00:00:00Z
updated: 2026-06-29T00:00:00Z
---

> **Foundation:** the core is **Rust + Wasmtime** running WebAssembly
> **Components** (WIT contracts), and runs as a standalone, user-privilege
> process — see
> [decisions/2026-06-29-component-model-rust](../decisions/2026-06-29-component-model-rust/Handoff.md).
> The agent loop, taxonomy, and contracts are unchanged from the Go + Wazero
> design; what moved is the host language/runtime, and — because **nothing is
> trusted** — the removal of the native/in-core extension tier (`api-*`/`chat-*`
> are now sandboxed WASM; UIs are separate clients). Items still being re-decided
> for Rust are marked **(TBD)**.

## Philosophy

Linux kernel model: the **core** is a minimal container with no domain logic. All agent behaviour is provided by **extensions** loaded at runtime.

**Design rules (standing):**
- **KISS** — every extension does one thing. If it grows a second responsibility, split it.
- **YAGNI** — do not add capability until there is a concrete use case. Pluggability is not an excuse to over-engineer.

Core runs as a **standalone process under the user's own privileges** (not a
system daemon) and is **headless-capable** — on a Raspberry Pi or in a container
it is the only thing you run. It contains:

- Extension lifecycle management (load, enable, disable, unload)
- Configuration loading (`jan-klod.yaml`)
- WASM component host (Wasmtime) — the capability sandbox every extension runs in
- Event bus (extension-to-extension communication)
- Observability (structured logging, Prometheus metrics, OpenTelemetry traces)

Zero agent behaviour in core. A core-only boot starts up and does nothing — the
agent loop itself is the `manager-agent-loop` **extension**, not core. The HTTP
API, UIs, and chat integrations are likewise extensions or external clients,
never built in.

## Extension model

Extensions are **WASM components** (`.wasm` files) dropped into `ext/`. They are loaded at runtime by Wasmtime and sandboxed — they can only do what the WIT interface explicitly grants. Because they are Component-Model components, each can be authored in **any `wit-bindgen` language** (Rust, JS, Python, Go, …) and all are interchangeable against the same WIT contract. See [Contracts](contracts.md) for the interface definitions.

### What the host grants extensions

- Outbound HTTP requests (to call LLM APIs, web search, etc.) — `host-http`
- **Inbound network listeners** (so `api-*` can serve REST/gRPC) — `host-serve` *(planned)*
- **Long-lived sockets** (so `chat-*` can hold a Telegram/Slack connection) — `host-socket` *(planned)*
- Storage read/write via the `memory-store` contract
- Logging, config read (own section only), event bus publish/subscribe

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
| `api-*` | Network API surfaces | WASM (`host-serve`) | `api-rest`, `api-grpc`, `api-graphql` |
| `chat-*` | Chat platform integrations | WASM (`host-socket`) | `chat-slack`, `chat-telegram`, `chat-whatsapp`, `chat-mattermost` |

**Every extension is a sandboxed, language-agnostic WASM component — nothing is trusted and nothing is compiled into core.** Extensions that need the network (`provider-*`, `tool-*`, `api-*`, `chat-*`) get it *only* through host-granted capabilities, never raw OS access. `api-*` and `chat-*` are therefore ordinary plugins: the user enables whichever `api-*` surface they want (or none) and any `chat-*` integrations they want (or none).

**User interfaces are not extensions.** TUI/GUI/web are optional, *separate client processes* that connect to core over an `api-*` HTTP+SSE surface (the LSP model: core is the server, the UI is a thin client). They are covered in [User interfaces](#user-interfaces-separate-clients) below.

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

api-rest              requires: agent-manager; uses host-serve (exposes core over HTTP + SSE)
api-grpc              requires: agent-manager; uses host-serve
api-graphql           requires: agent-manager; uses host-serve

chat-slack            requires: agent-manager; uses host-socket
chat-telegram         requires: agent-manager; uses host-socket
chat-whatsapp         requires: agent-manager; uses host-socket

# UIs are NOT extensions — separate client processes that connect over api-rest (HTTP+SSE)
```

### User interfaces (separate clients)

UIs are **not extensions and are not part of core.** They are optional, separate
**client processes** that connect to a running core over an `api-*` HTTP+SSE
surface — the same way an editor talks to a language server. Core never embeds a
UI; a headless deployment (Raspberry Pi, container, Telegram-only) runs no UI
client at all.

A single client binary presents either a terminal or a graphical UI depending on
how it is launched:

| Launch | Surface | Technology |
|---|---|---|
| `jan-klod-ui` (default) | Terminal UI | Rust TUI toolkit **(TBD — e.g. `ratatui`)** |
| `jan-klod-ui --gui` | Native window | Rust desktop/WebView shell **(TBD — e.g. Tauri)** |
| browser → `api-rest` | Web UI | served by the `api-rest` extension; open a browser tab |

All three are clients of the same `api-*` surface, so they share one backend and
carry no agent logic. *(Open: whether core also exposes a small built-in local
control endpoint so a UI client can attach to a bare core with no `api-*`
enabled, or whether a UI deployment always includes `api-rest`. Current lean:
require `api-rest`, matching the LSP/server model.)*

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

The HTTP surface is **not in core** — it is provided by an `api-*` extension
(e.g. `api-rest`) that binds a listener through the host `host-serve` capability
and exposes core over **REST + Server-Sent Events (SSE)**. UI clients, browsers,
and remote ACP callers all consume this surface. Curl-debuggable,
browser-compatible, no stub generation. Rust HTTP framework inside the extension
**(TBD — e.g. `axum`)**.

## Storage

| Extension | Backend | Notes |
|---|---|---|
| `store-sqlite` | SQLite | Default — zero-ops; Rust SQLite library **(TBD — `rusqlite` bundled vs. pure options)** |
| `store-postgres` | PostgreSQL | Self-hosted, multi-user; Rust driver **(TBD — e.g. `sqlx`/`tokio-postgres`)** |
| `store-supabase` | Supabase | Hosted Postgres + realtime + auth |

SQL layer: type-safe Rust SQL **(TBD — e.g. `sqlx` compile-time-checked queries)**.

The persistent store is a **host-side capability** the core exposes through the
`memory-store` / `host-storage` contract — it is *not* SQLite-in-wasm (which the
Go MVP confirmed does not work). Only one `memory-store` is active at a time;
selected via `jan-klod.yaml`.

## Stack

| Layer | Technology |
|---|---|
| Core language | Rust |
| Process model | `core` = standalone process under the user's privileges, hosting the WASM sandbox; UI clients connect over an `api-*` HTTP+SSE surface |
| WASM host | Wasmtime (Rust-native, no CGo) |
| Extension format | WASM Component Model + WIT interfaces (`wit-bindgen`) — every extension, incl. `api-*`/`chat-*` |
| HTTP surface | provided by `api-*` extensions (REST + SSE) via the `host-serve` capability; framework **(TBD — e.g. `axum`)** |
| SQL (host-side) | type-safe Rust SQL **(TBD — e.g. `sqlx`)** |
| UI clients (separate, optional) | one client binary: TUI default, GUI by launch flag, web via browser — toolkits **(TBD)** |
| Build | Cargo (native binary; no CGo in the core) |
| Linting | Clippy (Rust core); `golangci-lint` for any Go-language tooling/guests |
| Observability | Structured logging + Prometheus + OpenTelemetry |
| Config | YAML (`jan-klod.yaml`) |
| Updater/supervisor | TinyGo standalone binary (blue/green flip + rollback) |

## Testing

- **Unit:** standard Rust `cargo test`
- **Integration:** Rust tests with real SQLite + embedded Wasmtime
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
| ARM home server / NAS | Low memory footprint (Rust + WASM); **headless core, no UI client** — e.g. `chat-telegram` for access, optionally `api-rest` |
| Docker | Single container; config via environment variables or mounted `jan-klod.yaml` |
| Kubernetes | Enterprise; horizontal scaling of stateless API layer; shared `store-postgres` or `store-supabase` |

## Deployment modes

- **Standard:** the `core` binary + `ext/*.wasm` + `jan-klod.yaml` (the deploy unit). A UI client binary is a separate, optional artifact.
- **Bundle:** pre-packaged ZIP with core + a curated `.wasm` set + pre-filled config; UI-oriented bundles also include the UI client binary.

See [Configurator](configurator.md) for generating these archives and [Blue/Green Deployment](blue-green-deployment.md) for the update strategy.
