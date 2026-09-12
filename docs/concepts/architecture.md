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
L3 Protocol       one versioned command/event schema (built) over stdio JSON-RPC (built) | REST + SSE (built, a projection) | WebSocket (Phase 13c)
L2 Extensions     provider | tool | interceptor | registry | agent | chat        (WASM Components, polyglot — built)
L1 Capabilities   host-fs | host-process | host-http | host-storage | host-config | host-log | host-event   (default-deny, built)
                  + effect sandbox behind host-process: Seatbelt on macOS (built) | Landlock on Linux (built) | Windows a spike (15d); manifest-declared grants (Phase 16)
L0 Kernel         lifecycle | capability broker | loop conductor | SQLite store (built) | session event log (Phase 14) | protocol server (Phase 13)
```

- **L0 stays boring** — mechanism, no policy. The only planned additions are the
  protocol server and the event log; both are host-side because both hold the
  session, the most sensitive thing the runtime has.
- **L1 is the syscall layer.** Each capability is default-deny and granted per
  component (today in `config.yaml`; from Phase 16 also declared in a manifest the
  host cross-checks against the component's real imports).
- **L2 keeps the taxonomy below.** Nothing "smart" lives anywhere else.
- **L3 is the ABI for clients**, and since Phase 13 a versioned contract of the
  same rank as WIT, negotiated at connect. Two transports carry it — stdio
  JSON-RPC and REST + SSE — and REST + SSE is one projection of it rather than a
  second contract.
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
**client processes** that reach a core over its client surface (see
[Transport](#transport)) — the same way an editor talks to a language server,
either by spawning one or by connecting to one already running. Core never
embeds a UI; a headless deployment (Raspberry Pi, container, Telegram-only) runs
no UI client at all. Since Phase 3 the surface is built into
the core binary, so a UI client attaches to any running core; the earlier open
question (a separate `api-rest` guest vs. a built-in endpoint) is closed.

| Launch | Surface | Technology | Status |
|---|---|---|---|
| `jan-klod` (default) | Terminal UI | `ratatui`, over stdio JSON-RPC — it spawns the gateway; `--addr` drives a running one over REST + SSE instead | built |
| browser → core `/` | Web UI | a static, dependency-light TypeScript front-end served by the core over **REST + SSE** (not WebSocket — see below) | **built** (#118 client, #119 serving) |
| `jan-klod --gui` | Native window | a **Tauri shell around the same web front-end** — system webview, not a third client codebase | **built** (#142; see below) |
| editor | IDE integration | `jan-klod-gateway acp` — ACP agent side on stdio, mapped onto the same turn path | **built** (18a is MCP, 18b is this) |
| other agents | MCP server | `jan-klod-gateway mcp` — `ask`, `session_list`, `session_get` over MCP stdio | **built** |

#### The window is a separate workspace, and that is a supply-chain decision

`jan-klod --gui` opens the **same** front-end the browser gets — the one the core
serves at `/` — in a system webview. There is no third client codebase, which is
the whole of Vision decision 5.

What the row above does not show is where the code lives. The Tauri shell is
`src/gui`, **its own cargo workspace**, not a member of `src/core`. That is not
tidiness: Tauri resolves **256 packages** the host workspace does not otherwise
need, and as a member those would land in `src/core/Cargo.lock` (406 → 663) and
be resolved and built by every `cargo test`, every `cargo clippy --workspace`
and every CI run, whether or not anyone touched the window. Kept separate, they
are behind `make gui` and nothing else reaches them.

The cost was measured before the code was written ([#141]) because the answer
could have been "don't":

| | |
|---|---|
| Packages added to this repository | **+256** (after dedup against the host workspace) |
| Clean release build | 332 CPU-seconds, 838 MB of `target/` |
| Release binary | 9.6 MB (no bundled browser — the webview is the OS's) |
| `deny.toml` entries it required | **11** — 5 MPL-2.0 crate exceptions, 6 unmaintained-advisory ignores |

Those eleven are named crate by crate rather than widening the policy, and all
of them are structural: `wry`, the webview binding Tauri sits on, depends on
`dom_query` and `dirs` itself, so a thinner alternative pays the same licences.
`deny.toml` carries the reasoning and the scope note — if the shell is ever
dropped, that block goes with it.

Three consequences worth knowing:

- **Nothing on the default build path builds it.** `make all` and `make gate` do
  not. `make gui` builds it and stages the binary beside `jan-klod`, which is
  how the client finds it (sibling of the running executable, then `PATH`).
- **The supply-chain gates do cover it.** `make lockfile`, `make deny` and
  `make audit` each name all three workspaces. The tree the policy was widened
  for is not the one that escapes the policy.
- **It ships opt-in.** `make bundle GUI=1` adds the binary and suffixes the
  archive `-gui`; `install.sh --gui` asks for that archive. A client choice is
  orthogonal to a distribution, so this is a second axis rather than a fourth
  distribution — see `scripts/distributions/README.md`.

On macOS the webview is WKWebView and needs nothing installed. On Linux it is
`webkit2gtk-4.1`, a system package; `jan-klod --gui` names it when the shell
fails to start rather than falling back to the terminal.

[#141]: https://github.com/PromptPasture/jan-klod/issues/141

#### The window is handed the token, and only on the core's origin

The web client keeps the gateway token in `sessionStorage` and prompts for it on
a 401 (`src/web/src/api.ts`). A window launched by a client that *already has*
the token should not make the user retype it, so the shell seeds it with a Tauri
initialization script — and that makes the window a credential boundary rather
than a frame.

Two rules, both in `src/gui/src/main.rs` and both tested:

- The seed **checks `location.origin` first.** An initialization script runs in
  every frame the webview loads, so an unguarded one would hand the token to
  whatever a page embedded.
- Navigation **off the core's origin is refused** and handed to the system
  browser, so the guard is a second line rather than the only one.

`docs/concepts/security-model.md` carries the row and names the tests.

#### The web client's bundle is committed, and the alternative costs more than it looks

`src/web` builds to a 5.5 kB ES module with no runtime dependencies, and the
core embeds it with `include_str!` so the binary carries the page — there is
no directory to ship beside it. `include_str!` resolves at **compile time**,
which is what makes this a decision rather than a detail: the bundle is either
in the tree when `cargo build` runs, or it is not.

**Committed**, for two reasons.

The first is that the alternative is not "build it in CI" — it is **Node
becoming a dependency of building the core**. Every `cargo build`, `make gate`
and CI job would need `npm run build` to have run first, and none of them
install Node today. That is a large tax on everyone who never touches the web
client, paid so that one generated file is absent from git.

The second is that this repository already does this, three times, and has a
shape for it. `src/core/protocol/schema/protocol.schema.json` is generated and
committed with a drift test that regenerates and compares; `ext/*.manifest.toml`
are generated from each component's real imports and committed; `wit/wkg.lock`
likewise. "The tree carries no generated artifacts" is not a property this
repository has, so preserving it here would buy nothing while costing the first
reason.

**What the choice costs, stated rather than glossed.** A committed bundle can go
stale: someone edits `src/web/src` and forgets to rebuild, and the binary serves
the previous page — silently, because `include_str!` is happy either way.
`make web-dist-drift` closes that (#127): it rebuilds `src/web` into a temp tree
and diffs against what is committed, the same regenerate-and-compare the schema's
test uses. It works because esbuild's output turned out to be reproducible — the
committed bundle is built on macOS and a Linux runner rebuilds it byte for byte.

Unlike the schema's, though, it needs **Node**, so it is not on `make gate`'s
path; it runs in CI's lint-test job, which has Node, and a contributor without
Node is not asked to pass it locally. The guard is therefore narrower than its
three precedents: it catches a stale bundle on every push and every pull
request, but not at the moment someone commits one. That asymmetry is the real
price of this decision.

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

**It is no longer the only surface.** Phase 13 made the client surface a
versioned contract of the same rank as the WIT package — `jan-klod-protocol`,
with typed commands and notifications and a version negotiated at connect (see
[Contracts](contracts.md#ui--core-client-surface)) — and gave it a second
transport:

**stdio JSON-RPC** (`jan-klod-gateway rpc`, `jan_klod_core::rpc`) is what the TUI
now uses by default and what editors will use. Newline-delimited JSON-RPC 2.0 on
the process's own stdin and stdout: **no port, no token, nothing left running**,
because the client spawns the gateway and owns it. Stdout carries frames and
nothing else; every log line goes to stderr.

The two differ in one structural way worth knowing. Over REST, a mid-turn
confirmation is answered on a *second connection* and a cancel is the client
disconnecting. Over stdio there is one pipe, so a reader thread holds it while
the turn runs and hands frames to the loop between the turn's own events — which
is how `turn/cancel` gets read at all, and how `turn/follow-up` (steering a
running turn) becomes possible for the first time, REST having no way to deliver
one.

**Phase 13c — WebSocket — is deferred until Phase 17 needs it**
([#43](https://github.com/PromptPasture/jan-klod/issues/43)). The obstacle is the
socket, not the framing: this protocol needs a read that can time out (so a
client that goes *silent* cannot pin a turn open — the core is single-threaded,
so that wedges the runtime, which is what `prompt_disconnect.rs` guards) and a
read that can run while a write does (so a mid-turn `turn/cancel` is readable).
`tiny_http`'s `Request::upgrade` returns the two halves **fused**, with no
`try_clone` and no `set_read_timeout`, and the private accessors it builds that
from are not exposed. The ways out — a second listener, or a listener the core
owns that serves both — are decisions best made by the client that will use one,
and that client is not started. REST + SSE stays as one projection of the same
contract, not a second contract. Vision
[decision 1](../decisions/2026-09-08-harness-platform-vision/Vision.md#decisions);
plan in the [roadmap](roadmap.md#phase-13--client-protocol).

### The MCP port (`jan-klod-gateway mcp`)

A third surface, and the first one this core does not define: **MCP's stdio
transport is the framing `rpc` already speaks** — newline-delimited JSON-RPC
2.0, frames on stdout and nothing else, logs on stderr. So `jan_klod_core::mcp`
is a method-name-and-payload adapter over the same envelope, not a second
transport, and it costs no dependency.

`rmcp`, the official Rust SDK, is async on tokio. This core is deliberately
synchronous — the agent session is `!Send` and lives on one thread, which is
also why `tiny_http` was chosen over `axum` — so adopting it would be an
architectural change dressed as a convenience.

Three tools: `ask` (one turn, the answer as text), `session_list` and
`session_get`, the last two reading the same payloads the REST surface serves so
an editor and a browser cannot disagree about what a session is.

Two things differ from the `rpc` surface, and both are MCP's shape rather than
ours:

- **A frame may have no `id`.** `notifications/initialized` arrives right after
  the handshake and earns no answer. `rpc` refuses a null id on purpose — an
  uncorrelatable answer is worse than none — and carrying that rule across would
  have rejected the first thing every client sends.
- **A failed tool is a *successful* response carrying `isError: true`.** That is
  the opposite of `protocol::jsonrpc`'s `Outcome`, which makes result and error
  mutually exclusive so "both" and "neither" are unrepresentable. Mapping a
  refused turn onto a JSON-RPC error would make every permission refusal read to
  an editor as a broken server.

The driver is **headless by construction**: stdin here carries protocol frames,
so anything that prompted would read a client's next request as the answer to a
confirmation. Its default answer is a refusal, so the right policy and the right
protocol behaviour come from one choice — an editor could not have answered
anyway. `host/tests/it/mcp.rs::a_write_requiring_turn_is_refused_and_nothing_is_written`
asserts that on the *effect*: the model asks to write a real path and the file
is not there afterwards.

**No security-model row, deliberately.** This surface grants nothing new — it
re-exposes the existing turn path behind the same permission gate, and the test
above is the evidence rather than the claim. A row asserting a boundary that
nothing separately enforces would be worse than no row; recorded here so that
"no row" is a decision rather than an omission.

### The ACP port (`jan-klod-gateway acp`)

The editor side, and **the first surface where the core is a JSON-RPC client as
well as a server on one pipe**. Everything else it serves is client→server
requests plus server→client notifications; ACP has the agent originate
`session/request_permission` and block on the editor's answer.

That is why this port needed `rpc`'s reader-thread shape and the MCP port did
not: while a turn runs, the serving thread is inside the conductor, so something
else has to be holding the pipe or the answer could not arrive until the turn it
unblocks had already finished.

Three things differ from MCP beyond the direction:

- **The version is an integer** (`protocolVersion: 1`), where MCP's is a date
  string. Three schemes now coexist and none may be copied into another.
- **`session/new` mints the session id**, so a client must read it back before
  it can prompt. `acp::Connection` is therefore drivable frame by frame rather
  than being only a read loop.
- **`stopReason` is not `isError`.** It says why a turn *ended*:
  `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`. A
  permission refusal is **`end_turn`** — `refusal` means the agent declined the
  whole exchange and the spec lets an editor discard the user's prompt, so
  reporting a blocked tool call that way would throw away what the user typed.
  A turn that genuinely failed has no stop reason and is a JSON-RPC error: the
  opposite placement from MCP, where a failure rides in-band.

**The editor's `cwd` is reported, not adopted.** `session/new` carries it, and
honouring it would let a client choose what the file tools may reach — the
workspace is a grant, and `Workspace::open` already refuses `$HOME` and
filesystem roots. It is echoed in `_meta` so a mismatch is visible.

**A disconnected editor needs no timeout.** Over REST an answer arrives on a
second connection, so a vanished client is invisible until a deadline expires.
Here it arrives on the same pipe: the channel closes on EOF and the parked read
returns at once, which is detection rather than waiting. A closed pipe refuses,
which is the safe end of it.

## Command sandbox

`host-process` bounds the *caller* — default-deny, a cwd jailed to the
workspace, a timeout, an output cap, a scrubbed environment. Confining what the
command itself *does* is a separate mechanism, and it is per-OS.

**macOS: Seatbelt, and it is deprecated.** `man sandbox-exec` opens with
"execute within a sandbox (DEPRECATED)", and its description repeats it. It is
used anyway, because it is what macOS gives an unprivileged process: no
entitlement, no helper, no root. There is no unprivileged replacement, and the
alternatives are this or nothing — which is the same conclusion Codex and Claude
Code reached. Every command is rebuilt as
`sandbox-exec -p <profile> -- <command>` with a profile generated from
`execution.sandbox`: `(deny default)`, reads allowed, writes only under the
paths `writable` names, network only if the policy says so.

**Linux: Landlock, and no `unsafe` to get it.** Landlock restricts *the calling
process*, so the obvious shape is to apply it between `fork` and `exec` —
`pre_exec`, which is an `unsafe fn` this workspace's lints forbid. Instead the
command is rewritten as `<gateway> confine --writable <dir> [--network] --
<command>`, and that child applies the ruleset **to itself** and then `exec`s
the command, becoming it. `exec` is safe, so the whole path is. The ruleset asks
for Landlock ABI 1's filesystem rights as a hard requirement — without them
there is no confinement to speak of — and newer rights best-effort; denying the
network is a hard requirement again, because it needs ABI 4 (kernel 6.7) and a
kernel that cannot do it must not be reported as having done it. Anything short
of `FullyEnforced` refuses the command.

The two backends therefore share a shape: **both replace the program with a
wrapper that confines and then becomes the command.** That is why
`SandboxBackend::confine` takes and returns an owned `Command` rather than
borrowing one — a `Command`'s program can be read but not changed.

**The fallback, on any platform and for any reason, is `approval-only`, and it
is stated rather than assumed.** A platform with no backend, or a macOS without
`/usr/bin/sandbox-exec`, resolves to approval-only and the boot line says which
of those it is — the runtime does not claim confinement it does not have. An
operator who would rather have *no* command than an unconfined one sets
`require: true`, which denies `host-process` outright instead of degrading. The
grants and the tests behind each of these are the "Command effects" row in the
[security model](security-model.md#capabilities).

**A long-lived child goes through exactly this, and that is the point.** A stdio
MCP server, an `lsp-*` or a browser driver has to be *held open*, which
`host-process`'s `exec` cannot do — it runs to completion. `spawn` holds one
instead, and it reaches the process through the same `ProcessRunner::prepared`
that `exec` does: confine, then cwd, environment and pipes, then start. A child
that outlives its call is more exposed than one that does not, so it gets the
same policy rather than a relaxed one, and the single place that decides how a
command is bounded is the single place both go through.

What differs is the grant and the lifetime. The grant, `execution.long-lived`,
**names processes** rather than permitting spawning: a guest asks for a name and
the host supplies the command, so it can start what an operator wrote down and
nothing else. The lifetime is the host's: the child is owned by the extension
instance that started it and killed when that instance goes — including on
gateway exit, and whether or not the guest ever asks. A guest that forgets, or
that traps before it can ask, leaves nothing behind.

Two things Seatbelt here does not do, both deliberate: **reads are not confined**
(the gap being closed is over effects, and a command that cannot read its
toolchain does not run), and a command needing a **Mach service** fails rather
than running unconfined — visible in the command's own error, and the right
direction to fail in.

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

### Compile cache

Unrelated to the store, but configured in the same `storage:` block:
`cache-dir` (default `wasmtime-cache`, beside `config.yaml`) is where Wasmtime
caches compiled components ([#60](https://github.com/PromptPasture/jan-klod/issues/60)),
so a second boot skips Cranelift instead of recompiling every guest. This is
Wasmtime's own built-in cache (`Cache`/`CacheConfig`, `Config::cache`) wired to
that directory at `Runtime::boot` — not a bespoke `.cwasm` cache — evaluated
first per the issue and adopted because it already keys on everything that
affects codegen (component bytes, target triple, compiler/ISA flags, Wasmtime
version) and treats a corrupt or foreign artefact as a miss, never an error.
See `core/src/wasm_cache.rs` for the evaluation and
[Security model](security-model.md) for what a cache hit trusts.

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

Logging is best-effort: a store failure is reported once per turn and never
cancels the turn, because a full disk should not be able to stop a conversation.

**The log is the only record of a session.** `entries` held a `{user, answer}`
row per turn until Phase 14b; nothing writes one now, and a second copy of the
same session in a second format is the thing that phase existed to remove.

### The envelope, and why it has its own version

Every `payload` is `{"v": <EVENT_LOG_VERSION>, "data": {…}}`. `EVENT_LOG_VERSION`
(`core::event_log`) is **not** the client protocol's version, and merging them
would be a mistake: a stored log outlives the clients that watched it happen, so
"can this build read this row?" and "can this client talk to this core?" are
different questions with different answers. A reader refuses a version newer
than its own rather than guessing at the shape, and accepts older ones.

### Everything else is a projection of it

`core::projection::transcript` turns rows into the `Vec<Message>` a turn
replays, and it is a pure function — no store, no I/O. Each row is placed the
way `conductor::run_turn` places it rather than by a rule invented for the
projection: a `follow-up` is a user message because that is how steering is
injected, a `tool-result` is a `Role::Tool` message carrying the call's id
because that is how the loop feeds a result back. `text-delta` rows are dropped
in favour of `done` (the conductor's own words: deltas are a non-authoritative
preview), so an agentic turn's *intermediate* assistant texts are not in the
transcript — the same as the `{user, answer}` transcript this replaces.

An `ask` and its answer are dropped too, and that is the load-bearing decision:
the prompt is put to the *user* by an interceptor over a channel the model has
no part in, so rendering the question as an assistant message would put words in
the model's mouth and the answer as a user message would make a permission click
look like something the user said.

Resume reads the projection bounded to the last `REPLAYED_TURNS` turns, counted
over `user-message` rows — turn boundaries, not row counts, since a turn is a
variable number of rows and a row bound would open a conversation with a tool
result answering a call the model cannot see. `GET /session/:id` serves the
projection as `messages`, `GET /sessions` previews it, and `POST
/session/:id/fork` copies a prefix into a new session that then diverges,
renumbered from 1 and holding no link back.

### Upgrading a database written before the log

A one-shot conversion runs at boot (`event_log::migrate_transcripts`, called
from `Runtime::open_store`) and turns each old `turn-N` row into a
`user-message` and a `done`, dated with the row's own timestamp so an old
session is not re-dated to the upgrade. It is idempotent — a session that has
any events is skipped — and the old rows are left in place, because deleting
them would make the conversion unrepeatable and irreversible in one step.

Required rather than optional: with the read surfaces on the log, an
unconverted database has sessions that are neither listed nor readable. What
cannot be recovered is what the old format never held — tool calls, warnings,
the `ask` and its answer — so a migrated turn is exactly two events, and its
`done` carries `agentic: false` because the transcript did not record it.

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
