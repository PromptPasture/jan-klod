---
type: concept
title: Architecture
description: High-level architecture of the Jan-Klod agent runtime
tags: [architecture, core, extensions, rust, wasm, wasmtime]
created: 2026-06-28T00:00:00Z
updated: 2026-09-09T00:00:00Z
---

> **Foundation:** the core is **Rust + Wasmtime** running WebAssembly
> **Components** (WIT contracts), and runs as a standalone, user-privilege
> process — see
> [decisions/2026-06-29-component-model-rust](../decisions/2026-06-29-component-model-rust/Handoff.md).
> The taxonomy and contracts are largely unchanged from the Go + Wazero design;
> what moved is the host language/runtime, and — because **nothing is trusted** —
> the removal of the native/in-core extension tier (`api-*`/`chat-*` are now
> sandboxed WASM; UIs are separate clients). The **agent loop** was re-architected
> on 2026-07-01: its thin *mechanism* now lives in **core**, and every agent
> *decision* is a sandboxed **interceptor** extension — see
> [Thin Loop + Interceptor Middleware](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md).
> All implementation choices are resolved as of v0.1.0.
>
> **Post-v0.1 (2026-09-08):** jan-klod is framed as an **agent runtime** — kernel
> + distributions + clients — in
> [Vision — Harness as a Platform](../decisions/2026-09-08-harness-platform-vision/Vision.md),
> phased as roadmap Phases 13–18: the client protocol as a first-class versioned
> contract, an event-sourced session log, an OS-level effect sandbox for
> `host-process`, capability manifests + a signed registry, a web client with a
> Tauri shell, MCP + ACP in both directions. Items below marked *(planned, Phase N)*
> come from there; everything else describes what is built.

## Philosophy

Linux kernel model: the **core** is a minimal container with no domain logic. All agent behaviour is provided by **extensions** loaded at runtime.

**Design rules (standing):**
- **KISS** — every extension does one thing. If it grows a second responsibility, split it.
- **YAGNI** — do not add capability until there is a concrete use case. Pluggability is not an excuse to over-engineer.

Core runs as a **standalone process under the user's own privileges** (not a
system daemon) and is **headless-capable** — on a Raspberry Pi or in a container
it is the only thing you run. It contains:

- Extension lifecycle management (load, enable, disable, unload)
- Configuration loading (`config.yaml`) — see [Configuration](configuration.md)
- WASM component host (Wasmtime) — the capability sandbox every extension runs in
- Event bus (extension-to-extension communication, observation-only)
- **Agent loop mechanism** — the thin conductor (`stream → tools → loop`) plus the
  **interceptor-chain dispatch** (see [Agent loop architecture](#agent-loop-architecture))
- Observability (structured logging, Prometheus metrics, OpenTelemetry traces)

**Zero agent *behaviour* in core.** The loop is pure *mechanism* — it holds no
policy. Every decision (should we enter the agentic loop, which model, which
tools, how to trim history, may this tool run) is made by a sandboxed
**interceptor** extension the loop consults. A core-only boot with no interceptors
runs a bare `stream → tools → loop` and nothing more. The HTTP API, UIs, and chat
integrations are likewise extensions or external clients, never built in.

> **Why the loop is in core (not an extension).** Once every decision moves to an
> interceptor, the loop has no behaviour left — it is ~200 lines of conductor
> (provider call, tool dispatch, interceptor dispatch, streaming handles, cancel,
> steering queue). Making that native Rust keeps "nothing trusted" intact (core
> *is* the trusted host; all providers/tools/stores/interceptors stay sandboxed
> WASM) while avoiding a WASM loop driving a WASM interceptor chain across the
> component boundary twice. Cost accepted: the loop conductor is no longer a
> swappable, polyglot component. See
> [the decision](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md).

## Layers

The system read top-down, as the [vision](../decisions/2026-09-08-harness-platform-vision/Vision.md#architecture--five-layers)
fixes it. Built and planned parts share one picture so the target is visible from
the current state:

```
L4 Clients        TUI (ratatui, built) | Web client + Tauri shell (Phase 17) | IDE via ACP (Phase 18) | chat channels (Telegram, built)
L3 Protocol       REST + SSE (built) → one versioned command/event schema over stdio JSON-RPC / WebSocket / SSE (Phase 13)
L2 Extensions     provider | tool | interceptor | registry | agent | chat        (WASM Components, polyglot — built)
L1 Capabilities   host-fs | host-process | host-http | host-storage | host-config | host-log | host-event   (default-deny, built)
                  + OS-level effect sandbox behind host-process (Phase 15); manifest-declared grants (Phase 16)
L0 Kernel         lifecycle | capability broker | loop conductor | SQLite store (built) | session event log (Phase 14) | protocol server (Phase 13)
```

- **L0 stays boring** — mechanism, no policy. The only planned additions are the
  protocol server and the event log; both are host-side because both hold the
  session, the most sensitive thing the runtime has.
- **L1 is the syscall layer.** Each capability is default-deny and granted per
  component (today in `config.yaml`; from Phase 16 also declared in a manifest the
  host cross-checks against the component's real imports).
- **L2 keeps the taxonomy below.** Nothing "smart" lives anywhere else.
- **L3 is the ABI for clients.** Today REST + SSE; Phase 13 makes it a versioned
  contract of the same rank as WIT, with REST + SSE kept as one projection.
- **L4 holds no state the core does not.** Every client is a projection of the
  session over L3.

## Extension model

Extensions are **WASM components** (`.wasm` files) dropped into `ext/`. They are loaded at runtime by Wasmtime and sandboxed — they can only do what the WIT interface explicitly grants. Because they are Component-Model components, each can be authored in **any `wit-bindgen` language** (Rust, JS, Python, Go, …) and all are interchangeable against the same WIT contract. See [Contracts](contracts.md) for the interface definitions.

### What the host grants extensions

- Outbound HTTP requests (to call LLM APIs, web search, etc.) — `host-http`
- **Inbound network listeners** (so `api-*` can serve REST/gRPC) — `host-serve` *(planned)*
- **Long-lived sockets** (so `chat-*` can hold a Telegram/Slack connection) — `host-socket` *(planned)*
- Storage read/write via `host-storage` — **granted, not ambient** (`persist: true`), and namespaced to the calling component
- Logging, config read (own section only), event bus publish/subscribe

### What extensions cannot do

Asserted by `host/tests/it/sandbox_boundary.rs`, which drives `tool-escape-probe` —
a component written to attempt each of these with plain `std`, rather than to ask
politely through a typed import:

- Direct filesystem access
- Open arbitrary network connections (`wasi:sockets` is wired into the linker;
  every address is refused)
- Read the host's standard input — the terminal `jan-klod-gateway ask` runs in
- Talk to other extensions directly (routed through core)
- Access host OS or process

### Extension taxonomy

| Category | Class | Mechanism | Examples |
|---|---|---|---|
| `provider-*` | LLM API clients | WASM | `provider-openai`, `provider-ollama`, `provider-anthropic` |
| `interceptor-*` | Agent-loop decision hooks | WASM (exports `interceptor`) | `interceptor-intent-router`, `interceptor-task-router`, `interceptor-tool-selector`, `interceptor-context`, `interceptor-permission` |
| `registry-*` | Capability catalogues | WASM | `registry-skills`, `registry-mcp` |
| `tool-*` | Discrete callable tools | WASM | `tool-web-search` |
| `agent-*` | AI agent delegation via ACP | WASM | `agent-claude-code`, `agent-opencode`, `agent-codex` |
| `api-*` | Network API surfaces | WASM (`host-serve`) | `api-rest`, `api-grpc`, `api-graphql` |
| `chat-*` | Chat platform integrations | WASM (`host-socket`) | `chat-slack`, `chat-telegram`, `chat-whatsapp`, `chat-mattermost` |

**Every extension is a sandboxed, language-agnostic WASM component — nothing is trusted and nothing is compiled into core.** (The only agent code *in* core is the loop *mechanism*; it carries no policy — see [Agent loop architecture](#agent-loop-architecture).) Extensions that need the network (`provider-*`, `tool-*`, `api-*`, `chat-*`) get it *only* through host-granted capabilities, never raw OS access. `api-*` and `chat-*` are therefore ordinary plugins: the user enables whichever `api-*` surface they want (or none) and any `chat-*` integrations they want (or none).

**`interceptor-*` extensions** are the agent loop's decision hooks. Each exports the
one generic `interceptor` interface (`intercept` + `subscribed-phases`); the core loop
invokes them **natively** at a fixed, ordered list of **phases** and acts on the
returned `decision` — `proceed | replace | block | ask` — a synchronous, ordered
dispatch, distinct from the observation-only event bus, with no `host-hook` capability
for extensions to import. Adding a lifecycle point is a new `phase` enum case, never a
new function or world import, so the design favours **many narrow phases over few broad
ones**: each phase is a real state transition, making ordering between concerns
*structural* (the phase order) rather than a config-fragile contract inside one big
phase. **Ordering is not configurable** — across phases it follows the `phase` enum,
within a phase it follows deterministic extension load order; `config.yaml` only
**enables/disables** interceptors. Most of what the old monolithic `manager-agent-loop`
did — intent routing, task classification, tool selection, context compression — is now
a separate, independently enabled interceptor. The phase model also doubles as
**agent lifecycle hooks**: `onStart` → `session-start`, `onFinish` → `finalize`,
`onToolCall` → `tool-call`/`tool-result`, `onError` → `on-error` (see below).
Any lifecycle point an external observer would want to react to is already a
phase — write an interceptor for it. (**Provider fallback is the exception**:
because it re-issues the *same* failed request on another provider, it stays **core loop
mechanism** — see [Provider fallback](#provider-fallback) — not an interceptor.)

An interceptor may return **`ask`** — a question routed *through the loop to the attached
driver* (TUI, chat, `api-*`), which surfaces it in its own idiom; the loop suspends and
the host re-invokes the same interceptor once the answer returns. This is how a
rule-based permission gate can confirm with the user even though the UI is a *separate
client* — the interceptor never touches a UI.

**User interfaces are not extensions.** TUI/GUI/web are optional, *separate client processes* that connect to core over an `api-*` HTTP+SSE surface (the LSP model: core is the server, the UI is a thin client). They are covered in [User interfaces](#user-interfaces-separate-clients) below.

Jan-Klod speaks ACP both ways — as a client (`agent-*` extensions call other agents) and as a server (it can be called by other ACP orchestrators).

### Extension dependency graph

```
core agent loop      mechanism only (in core): stream → tools → loop, interceptor dispatch,
                     grammar passthrough, parse/validate/retry, streaming handles

provider-anthropic  ─┐
provider-openai     ─┤ implements llm-provider WIT interface
provider-ollama     ─┘

interceptor-intent-router   exports interceptor; phase @ before-loop     (simple vs agentic; may short-circuit)
interceptor-task-router     exports interceptor; phase @ select-model     (task classification → task→model routing)
interceptor-context         exports interceptor; phase @ select-context   (trim/compress history to the model budget)
interceptor-tool-selector   exports interceptor; phase @ select-tools     (which tools to expose)
interceptor-permission      exports interceptor; phase @ tool-call        (allow / replace / block / ask on a tool call)

# provider fallback is NOT an interceptor — it is core loop mechanism (re-issues the failed request)

# there is no store-* family: persistence is host-side, configured by the
# top-level `storage:` block. See "Storage" below.

registry-skills       implements skill-registry WIT interface
registry-mcp          implements mcp-registry WIT interface

tool-web-search       implements tool-callable; offered to the loop via interceptor-tool-selector

agent-claude-code     implements agent-delegate (delegates tasks via ACP)
agent-opencode        implements agent-delegate

api-rest              drives the core loop; uses host-serve (exposes core over HTTP + SSE)
api-grpc              drives the core loop; uses host-serve
api-graphql           drives the core loop; uses host-serve

chat-slack            drives the core loop; uses host-socket
chat-telegram         drives the core loop; uses host-socket
chat-whatsapp         drives the core loop; uses host-socket

# UIs are NOT extensions — separate client processes that connect over api-rest (HTTP+SSE)
```

### User interfaces (separate clients)

UIs are **not extensions and are not part of core.** They are optional, separate
**client processes** that connect to a running core over its host-side client
surface (see [Transport](#transport)) — the same way an editor talks to a language
server. Core never embeds a UI; a headless deployment (Raspberry Pi, container,
Telegram-only) runs no UI client at all. Since Phase 3 the surface is built into
the core binary, so a UI client attaches to any running core; the earlier open
question (a separate `api-rest` guest vs. a built-in endpoint) is closed.

| Launch | Surface | Technology | Status |
|---|---|---|---|
| `jan-klod-ui` (default) | Terminal UI | `ratatui`, over REST + SSE today; over stdio JSON-RPC from Phase 13 | built |
| browser → core `/` | Web UI | a static, dependency-light TypeScript front-end served by the core, speaking the protocol over WebSocket | planned, Phase 17 |
| `jan-klod-ui --gui` | Native window | a **Tauri shell around the same web front-end** — system webview, not a third client codebase | planned, Phase 17 |
| editor | IDE integration | ACP server side mapped onto the protocol | planned, Phase 18 |

All are clients of one surface, share one backend, and carry no agent logic. A
client holds no state the core does not: every view is a projection of the
session's event log (Phase 14) delivered over the protocol (Phase 13).

## Agent loop architecture

The loop is a thin **conductor in core**. It runs a fixed `stream → tools → loop`
cycle and, at an **ordered list of phases**, invokes the enabled `interceptor-*`
extensions natively (each exports the `interceptor` interface) and acts on their
`proceed | replace | block | ask` return. All *decisions* live in interceptors; the
loop itself holds only *mechanism* (grammar passthrough, parse, structural
validation, retry-with-correction, **provider fallback**, streaming handles, cancel,
and the steering/follow-up queue). The phases, in order:

```
Session opens
    │
    ▼
[phase: select-model]   interceptor-system — set the standing instructions
    │
    ▼
User query (from an api-*/chat-* driver)
    │
    ▼
[phase: before-loop]  interceptor-intent-router ──→ direct answer (skip agentic loop)
    │
    ▼
┌───────────────────────── core loop (mechanism) ─────────────────────────┐
│  [phase: select-model]    interceptor-task-router  — pick model          │
│  [phase: select-context]  interceptor-context      — trim to budget      │
│  [phase: select-tools]    interceptor-tool-selector — fix tool set       │
│      │                    (ordered phases: each hands a defined state on) │
│      ▼                                                                   │
│  LLM complete(request incl. grammar) — provider executes constrained     │
│      │                              decoding                             │
│      │   └─(provider error)─▶ fallback: next model/provider from         │
│      │                        `providers:` (CORE mechanism) ─▶ re-issue  │
│      ▼                                                                   │
│  [phase: after-response]  raw output repair / reasoning-strip / redact   │
│      │                                                                   │
│      ▼                                                                   │
│  Parse & structurally validate action  ──(malformed)──▶ retry+correction │
│      │                                                                   │
│      ▼ tool call?                                                        │
│  [phase: tool-call]  interceptor-permission ──→ allow / block / replace  │
│      │                                        └─ ask ─▶ driver prompts ──┐│
│      │                                     ◀── answer ── (loop resumes) ──┘│
│      ▼                                                                   │
│  Tool execution  ──▶ [phase: tool-result] modify ──▶ (terminate? stop)   │
│      │                    └─(error)──▶ [phase: on-error]  decide          │
│      │                                  retry / abort / log              │
│      │                                                                   │
│      ▼                                                                   │
│  [phase: prepare-next-turn]  (optional model/context swap) ──┐          │
│      └───────────────────── loop back ◀──────────────────────┘          │
└──────────────────────────────────────────────────────────────────────────┘
    │  (final-answer, or every tool result set terminate)
    ▼
[phase: finalize]  shape the authoritative answer (citations, redaction)
    │
    ▼
Authoritative answer emitted (text-delta … done)
```

(Tokens also stream live *during each turn's* `complete()` as a non-authoritative
preview; `finalize` shapes the authoritative copy the driver reconciles against.)

Streamed tokens are a **non-authoritative preview**; the loop emits the turn's
authoritative message at the boundary, which `after-response`/`finalize` may have
`replace`d — drivers reconcile the two (Pi's `message_update → message_end` model).
Mid-run **steering** messages and post-stop **follow-up** messages can be injected
by the driver; a tool result may set **`terminate`** to end the loop. An interceptor that returns **`ask`** suspends the loop until the driver answers.

The **`on-error`** phase fires when a non-recoverable error occurs (provider
exhaustion, tool-crash, validation failure after all retries). An interceptor
here may log, emit a user-visible message, or request a retry — but the loop
*has already* exhausted its internal retry logic before reaching this phase, so
`on-error` is for observability and graceful degradation, not for error recovery.

See
[Small-Model Harness](small-model-harness.md) for how the mitigation strategies map
onto these hooks, and
[the decision record](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md)
for the rationale.

## Transport

The HTTP surface is **host-side** (not an extension) — `jan_klod_core::serve`
runs a synchronous `tiny_http` listener and exposes core over **REST + Server-Sent
Events (SSE)**. UI clients, browsers, and remote ACP callers all consume this
surface. Curl-debuggable, browser-compatible, no stub generation. Endpoints:
`GET /health`, `GET /sessions`, `POST /sessions`, `GET /session/:id`,
`POST /session/:id/message` (SSE or JSON).

**Planned, Phase 13:** this surface becomes a versioned **client protocol** of the
same rank as the WIT contracts — a `protocol` crate with typed commands and
notifications, a protocol version negotiated at connect, and three transports:
stdio JSON-RPC (a gateway `rpc` subcommand, for the TUI and editors), WebSocket (for
the web client), and the existing REST + SSE kept as one projection. Vision
[decision 1](../decisions/2026-09-08-harness-platform-vision/Vision.md#decisions);
plan in the [roadmap](roadmap.md#phase-13--client-protocol).

## Storage

| Backend | Status | Notes |
|---|---|---|
| SQLite | **Shipping** | Zero-ops; `rusqlite` bundled, host-side (not SQLite-in-wasm) |
| PostgreSQL | Not built | Would be a second host backend behind the same `Store` type |

The persistent store is a **host-side capability** the core exposes through the
`host-storage` contract — it is *not* SQLite-in-wasm (which the Go MVP confirmed
does not work). It is configured by the top-level `storage:` block: `path` for a
durable file, absent for in-memory.

It is deliberately **not an extension**, and the `store-*` component family this
page used to list never existed. The sandbox has no filesystem, so a store guest
would need one granted back; and the conversation transcript is the most
sensitive thing the runtime holds, so the fewer parties that hold it the better.
A component that wanted swappable backends would be trading the one guarantee
this design exists to make for a plugin point nobody asked for. Postgres or
Supabase, if they arrive, arrive as host backends behind the same `Store` type.

### Two tables

`entries` is the key/value store `host-storage` serves: `(namespace, key)`,
opaque JSON values, updated in place. `events` is the **append-only turn log**:

```sql
events(session TEXT, seq INTEGER, ts INTEGER, kind TEXT, payload TEXT,
       PRIMARY KEY (session, seq))
```

One row per thing that happened, `seq` numbered from 1 **per session** so a
session's log reads as a sequence with no gaps. `seq` is allocated inside the
insert (`SELECT COALESCE(MAX(seq), 0) + 1 … RETURNING seq`), so it cannot claim
a number another append already took. There is no update and no single-row
delete: a log whose rows could be rewritten would make every replay a claim
about the present rather than the past. `purge_session_events` — forgetting a
whole session — is the one removal path.

What is logged, and by what: every `conductor::Event` via `PersistingSink`
(a fan-out, so the SSE stream and the TUI still receive everything); the message
that started the turn; and the `ask`, its answer and any steering follow-up via
`PersistingDriver`. The answer is recorded whatever its provenance, including the
default taken when nobody replied, because a replay cannot otherwise tell an
approval from a timed-out denial. Text deltas are stored one row per event and
**not** coalesced — a replaying client needs them as they arrived.

Logging is best-effort in the same sense the transcript append is: a store
failure is reported once per turn and never cancels the turn, because a full
disk should not be able to stop a conversation.

### The envelope, and why it has its own version

Every `payload` is `{"v": <EVENT_LOG_VERSION>, "data": {…}}`. `EVENT_LOG_VERSION`
(`core::event_log`) is **not** the client protocol's version, and merging them
would be a mistake: a stored log outlives the clients that watched it happen, so
"can this build read this row?" and "can this client talk to this core?" are
different questions with different answers. A reader refuses a version newer
than its own rather than guessing at the shape, and accepts older ones.

The transcript in `entries` is still what a new turn replays from; making
transcript, resume and fork projections *of this log* is
[Phase 14b](roadmap.md#phase-14--event-sourced-session-log).

## Stack

| Layer | Technology |
|---|---|
| Core language | Rust |
| Process model | `core` = standalone process under the user's privileges, hosting the WASM sandbox; UI clients connect over an `api-*` HTTP+SSE surface |
| WASM host | Wasmtime (Rust-native, no CGo) |
| Extension format | WASM Component Model + WIT interfaces (`wit-bindgen`) — every extension, incl. `api-*`/`chat-*` |
| HTTP surface | host-side `tiny_http` (sync); REST + SSE; `GET /health`, `GET /sessions`, `POST /sessions`, `GET /session/:id`, `POST /session/:id/message` |
| SQL (host-side) | `rusqlite` bundled; host-side store (not SQLite-in-wasm) |
| UI clients (separate, optional) | `jan-klod-ui`: TUI (`ratatui`); GUI via Tauri *(planned)*; web via browser |
| Build | Cargo (native binary; no CGo in the core) |
| Linting | Clippy (Rust core); `golangci-lint` for any Go-language tooling/guests |
| Observability | Structured logging + Prometheus + OpenTelemetry |
| Config | YAML (`config.yaml`) |
| Updater/supervisor | TinyGo standalone binary (blue/green flip + rollback) |

## Testing

- **Unit:** standard Rust `cargo test`
- **Integration:** Rust tests with real SQLite + embedded Wasmtime
- **Extension:** WASM component loaded in test harness, WIT interface verified

## Provider fallback

When a provider or model fails (unavailable, rate-limited, quota exceeded, local OOM), the **core loop** falls back through a two-level priority list defined in `config.yaml`, re-issuing the request against the next entry. Fallback is core *mechanism*, not an interceptor: it re-issues the *same* failed request on another provider (an on-provider-error retry, the same category as retry/validate), which a `prepare-next-turn` interceptor cannot do. Entries reference **provider instance names** (`extensions.provider.<name>`), not wasm components — see [Configuration](configuration.md):

```yaml
providers:
  - provider: anthropic
    models:
      - claude-sonnet-4-6
      - claude-haiku-4-5        # cheaper fallback within same provider
  - provider: openai
    models:
      - gpt-4o
      - gpt-4o-mini
  - provider: ollama            # local, always available
    models:
      - qwen2.5:14b
      - qwen2.5:7b              # smaller if 14b OOM
```

Fallback order: try each model within the current provider → move to next provider → if all exhausted, surface error to user (no silent retry spiral).

Fallback is per-request — if the primary recovers, the next request uses it again. This also enables cost routing: cheap tasks naturally route to smaller/cheaper models without a separate configuration.

## Model catalog

Several loop decisions need to know facts *about a model* that the model itself
doesn't report: its context-window size, what modalities it accepts, whether it
supports reasoning/thinking, and its token pricing. Jan-Klod keeps this as a small
**per-model catalog** — a lookup from model id to metadata:

| Field | Used by | For |
|---|---|---|
| `context-window` | `interceptor-context` (`select-context`) | the budget to trim/compress history against |
| `max-output` | core loop | capping `max-tokens` on the request |
| `modalities` (`text`, `image`, …) | `interceptor-task-router` (`select-model`) | routing only to models that can accept the input |
| `reasoning` (bool) | `interceptor-task-router` | routing reasoning-heavy task types to capable models |
| `cost` (input / output / cache) | `interceptor-task-router` | cost-aware routing — cheap tasks to cheap models |

The catalog is **data, not policy** — a refreshable table shipped with defaults and
overridable in `config.yaml`, read by the request-shaping interceptors through
`host-config`. It is what lets `select-context` size its budget and `select-model`
route by capability and cost without hard-coding model facts into the loop. New
models are added by extending the table, no code change.

## Task routing

`interceptor-task-router` classifies each request into a task type and routes it to the configured provider/model (setting the model on the outbound request at the `select-model` phase). Jan-Klod ships built-in task types as sensible defaults; users extend or override in `config.yaml`.

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

User-defined types can be added to `config.yaml` — the LLM classifier receives the full list at runtime and picks the closest match. No code changes needed to add a type.

Routing values are `<provider-instance>/<model>`, where the instance is a name under `extensions.provider`:

```yaml
routing:
  code-generation:   ollama/qwen2.5:14b
  code-review:       ollama/qwen2.5:14b
  file-edit:         ollama/qwen2.5:14b
  reasoning:         anthropic/claude-sonnet-4-6
  planning:          anthropic/claude-sonnet-4-6
  web-search:        openai/gpt-4o-mini
  chat:              anthropic/claude-haiku-4-5
  clarification:     anthropic/claude-haiku-4-5
  agent-delegation:  anthropic/claude-sonnet-4-6
  # user-defined:
  data-analysis:     openai/gpt-4o
```

## Parallel decomposition

> **Deferred (not in the v1 interceptor set).** Decomposition fans out multiple
> independent sub-runs and merges them — it does not fit the `proceed`/`replace`/`block`/`ask`
> decision shape, so it is a later addition (likely a dedicated `interceptor-decomposer`
> that drives child loop runs, or a driver-side concern). Recorded here as intent,
> not a Phase 2 commitment. See
> [open questions](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md#open-questions).

For tasks where subtasks are independent, a decomposition interceptor splits the
work and dispatches in parallel:

- **`file-edit`** — each file edited in parallel, results merged
- **`web-search`** — multiple queries in parallel, results merged before LLM synthesis
- **`research`** — multiple sources fetched in parallel
- **`code-review`** — each module reviewed independently

Single-subtask requests skip decomposition and route directly.

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
2. Remove its tools from the active tool set — `interceptor-tool-selector` will not offer them at the `select-tools` phase.
3. Emit an event on the bus — UI clients display a warning to the user.
4. Attempt reconnect on an exponential backoff timer.

No crash propagates to core. The agent loop continues with the remaining tools.

## Config hot-reload

Extensions can pick up `config.yaml` changes without restart. Core watches the config file and notifies affected extensions via the event bus. Extensions opt in to hot-reload by implementing the reload lifecycle hook.

## Deployment targets

| Target | Notes |
|---|---|
| Desktop (macOS, Windows, Linux) | Primary target; all UI modes available |
| ARM home server / NAS | Low memory footprint (Rust + WASM); **headless core, no UI client** — e.g. `chat-telegram` for access, optionally `api-rest` |
| Docker | Single container; config via environment variables or mounted `config.yaml` |
| Kubernetes | Enterprise; horizontal scaling of a stateless API layer would need a shared Postgres backend, which is not built |

## Deployment modes

- **Standard:** the `core` binary + `ext/*.wasm` + `config.yaml` (the deploy unit). A UI client binary is a separate, optional artifact.
- **Bundle:** pre-packaged ZIP with core + a curated `.wasm` set + pre-filled config; UI-oriented bundles also include the UI client binary.

See [Configurator](configurator.md) for generating these archives and [Blue/Green Deployment](blue-green-deployment.md) for the update strategy.
