---
type: concept
title: Architecture
description: High-level architecture of the Jan-Klod agent runtime
tags: [architecture, core, extensions, rust, wasm, wasmtime]
created: 2026-06-28T00:00:00Z
updated: 2026-09-09T00:00:00Z
---

> **Foundation:** **Rust + Wasmtime** host running WebAssembly Components (WIT contracts) as a standalone, user-privilege process — see [decisions/2026-06-29-component-model-rust](../decisions/2026-06-29-component-model-rust/Handoff.md). Taxonomy and contracts unchanged from Go + Wazero; host language/runtime and extension sandboxing differ. Because **nothing is trusted**, native extensions are gone; `api-*`/`chat-*` and UIs are now sandboxed WASM or separate clients. The **agent loop** (re-architected 2026-07-01) has thin *mechanism* in **core** and all *decisions* in **interceptor** extensions — see [Thin Loop + Interceptor Middleware](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md). All implementation choices resolved as of v0.1.0.
>
> **Post-v0.1 (2026-09-08):** jan-klod is an **agent runtime** (kernel + distributions + clients) — [Vision — Harness as a Platform](../decisions/2026-09-08-harness-platform-vision/Vision.md), Phases 13–18: versioned client protocol, event-sourced session log, OS-level effect sandbox for `host-process`, manifest-declared capability grants, web client + Tauri shell, MCP + ACP bidirectional. Items marked *(planned, Phase N)* come from there; others describe what is built.

## Philosophy

Linux kernel model: **core** is a minimal container with no domain logic; all agent behaviour comes from **extensions** loaded at runtime.

**Design rules:** **KISS** — each extension does one thing. **YAGNI** — add capability only when needed; pluggability is not an excuse to over-engineer.

Core runs as a **standalone user-privilege process** (not a daemon) and is **headless-capable** — Raspberry Pi or container runs only this. It contains:

- Extension lifecycle (load, enable, disable, unload)
- Configuration loading (`config.yaml`) — see [Configuration](configuration.md)
- WASM host (Wasmtime) — capability sandbox for extensions
- Event bus (extension-to-extension, observation-only)
- **Agent loop mechanism** — thin conductor (`stream → tools → loop`) + **interceptor dispatch** (see [Agent loop architecture](#agent-loop-architecture))
- Observability (structured logging, Prometheus, OpenTelemetry)

**Zero agent behaviour in core.** The loop is pure *mechanism* — no policy. Every decision (loop entry, model, tools, history trim, tool permit) comes from sandboxed **interceptor** extensions. A core-only boot with no interceptors runs bare `stream → tools → loop`. UIs are separate clients, and the kernel crate holds no transport at all: REST, stdio JSON-RPC, ACP and MCP live in the `jan-klod-host` binary crate that serves them ([#179](https://github.com/PromptPasture/jan-klod/issues/179)), and the Telegram poller sits beside them until [#173](https://github.com/PromptPasture/jan-klod/issues/173) makes it an extension.

> **Why the loop is in core (not an extension).** With every decision moved to interceptors, the loop is ~200 lines of conductor (provider call, tool dispatch, interceptor dispatch, streaming handles, cancel, steering queue). Native Rust keeps "nothing trusted" (core is the trusted host; all providers/tools/stores/interceptors stay sandboxed WASM) and avoids WASM loop driving WASM interceptor chain across component boundaries twice. Trade-off: the conductor is no longer swappable/polyglot. See [the decision](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md).

## Layers

The system, read top-down per the [vision](../decisions/2026-09-08-harness-platform-vision/Vision.md#architecture--five-layers), unifies built and planned parts so the target is visible from the current state:

```
L4 Clients        TUI (ratatui, built) | Web client + Tauri shell (Phase 17) | IDE via ACP (Phase 18) | chat channels (Telegram, built)
L3 Protocol       one versioned command/event schema (built) over stdio JSON-RPC (built) | REST + SSE (built, a projection) | WebSocket (Phase 13c)
L2 Extensions     provider | tool | interceptor | registry | agent | chat        (WASM Components, polyglot — built)
L1 Capabilities   host-fs | host-process | host-http | host-storage | host-config | host-log | host-event   (default-deny, built)
                  + effect sandbox behind host-process: Seatbelt on macOS (built) | Landlock on Linux (built) | Windows a spike (15d); manifest-declared grants (Phase 16)
L0 Kernel         lifecycle | capability broker | loop conductor | SQLite store (built) | session event log (Phase 14) | protocol server (Phase 13)
```

- **L0 stays boring** — mechanism, no policy. Planned additions (protocol server, event log) are host-side because both hold the session, the runtime's most sensitive asset.
- **L1 is the syscall layer.** Each capability is default-deny, granted per component (today in `config.yaml`; Phase 16 also in manifest, host-verified against real imports).
- **L2 keeps the taxonomy.** Nothing "smart" elsewhere.
- **L3 is the client ABI** — since Phase 13 a versioned contract ranked as WIT, negotiated at connect. Two transports: stdio JSON-RPC and REST + SSE (the latter a projection, not a second contract).
- **L4 holds no state the core lacks.** Each client is a session projection over L3.

## Extension model

Extensions are **WASM components** (`.wasm` files) in `ext/`, loaded at runtime by Wasmtime and sandboxed — they can only do what WIT explicitly grants. Each can be authored in **any language with a Component Model toolchain**, and all are interchangeable against the same WIT contract. See [Contracts](contracts.md) for interface definitions.

Three languages are proven rather than claimed, each by a guest the gate runs a turn through: **Rust** (`wit-bindgen`, every first-party extension), **TypeScript** (`jco`, `tool-hello-ts`) and **Python** (`componentize-py`, `tool-hello-py`), plus a TinyGo spike. Note that only Rust uses `wit-bindgen` — the other two toolchains generate or embed their own bindings, which is why the claim is about the Component Model and not about one binding generator. [Writing an extension](../guides/writing-an-extension.md#meet-the-cost-first) has what each language costs; the non-Rust components are 12.7 MB and 18.5 MB against Rust's 55 KB, so this is a real choice rather than a free one.

### What the host grants extensions

- Outbound HTTP requests (to call LLM APIs, web search, etc.) — `host-http`
- **Inbound network listeners** (so `api-*` can serve REST/gRPC) — `host-serve` *(planned)*
- **Long-lived sockets** (so `chat-*` can hold a Telegram/Slack connection) — `host-socket` *(planned)*
- Storage read/write via `host-storage` — **granted, not ambient** (`persist: true`), and namespaced to the calling component. A tool may add `scope: session` to key its namespaces by session as well, so two sessions do not share one tool's state; the default is run-scoped, which is what a permission gate's standing grants need (#215)
- Logging, config read (own section only), event bus publish/subscribe

### What extensions cannot do

Asserted by `host/tests/it/sandbox_boundary.rs`, which drives `tool-escape-probe` — a component written to attempt each with plain `std` rather than via typed import:

- Direct filesystem access
- Open arbitrary network connections (`wasi:sockets` wired, all refused)
- Read the host's standard input (the terminal `jan-klod-gateway ask` runs in)
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

**Every extension is a sandboxed, language-agnostic WASM component — nothing trusted, nothing compiled into core.** (Only loop *mechanism* is in core; it carries no policy — see [Agent loop architecture](#agent-loop-architecture).) Extensions needing network (`provider-*`, `tool-*`, `api-*`, `chat-*`) get it only through host-granted capabilities, never raw OS access. `api-*` and `chat-*` are ordinary plugins: users enable whichever surfaces/integrations they want (or none).

**`interceptor-*` extensions** are decision hooks. Each exports the generic `interceptor` interface (`intercept` + `subscribed-phases`); core invokes them **natively** at fixed, ordered **phases** and acts on returned `decision` (`proceed | replace | block | ask`) — synchronous, ordered dispatch, distinct from the observation-only event bus, with no `host-hook` import. New lifecycle points are `phase` enum cases, never new functions, so **many narrow phases** over few broad ones, making ordering *structural* (phase order) rather than config-fragile. **Ordering is not configurable** — across phases it follows the enum, within a phase it follows deterministic load order; `config.yaml` only enables/disables interceptors. The old monolithic `manager-agent-loop` (intent routing, task classification, tool selection, context compression) is now separate, independently enabled interceptors. **Agent lifecycle hooks**: `onStart` → `session-start`, `onFinish` → `finalize`, `onToolCall` → `tool-call`/`tool-result`, `onError` → `on-error`. Any external lifecycle event is a phase — write an interceptor for it. (**Provider fallback is core loop mechanism**, not an interceptor — it re-issues the same failed request on another provider; see [Provider fallback](#provider-fallback).)

An interceptor may return **`ask`** — a question routed through the loop to the attached driver (TUI, chat, `api-*`), which surfaces it in its idiom; the loop suspends and re-invokes the same interceptor with the answer. This lets rule-based permission gates confirm with the user even though the UI is separate — the interceptor never touches a UI.

**UIs are not extensions** — TUI/GUI/web are optional separate client processes connecting over `api-*` HTTP+SSE (LSP model: core is server, UI is thin client). See [User interfaces](#user-interfaces-separate-clients).

Jan-Klod speaks ACP bidirectionally — as a client (`agent-*` extensions call other agents) and as a server (callable by other ACP orchestrators).

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

UIs are **not extensions, not part of core.** They are optional separate **client processes** reaching core over its client surface (see [Transport](#transport)) — like an editor talking to a language server, either spawning one or connecting to one running. Core never embeds a UI; headless deployments (Raspberry Pi, container, Telegram-only) run no UI. Since Phase 3 the surface is built into the core binary, so UI clients attach to any running core (earlier question of separate `api-rest` guest vs. built-in endpoint is closed).

| Launch | Surface | Technology | Status |
|---|---|---|---|
| `jan-klod` (default) | Terminal UI | `ratatui`, over stdio JSON-RPC — it spawns the gateway; `--addr` drives a running one over REST + SSE instead | built |
| browser → core `/` | Web UI | a static, dependency-light TypeScript front-end served by the core over **REST + SSE** (not WebSocket — see below) | **built** (#118 client, #119 serving) |
| `jan-klod --gui` | Native window | a **Tauri shell around the same web front-end** — system webview, not a third client codebase | **built** (#142; see below) |
| editor | IDE integration | `jan-klod-gateway acp` — ACP agent side on stdio, mapped onto the same turn path | **built** (18a is MCP, 18b is this) |
| other agents | MCP server | `jan-klod-gateway mcp` — `ask`, `session_list`, `session_get` over MCP stdio | **built** |

#### The window is a separate workspace, and that is a supply-chain decision

`jan-klod --gui` opens the **same** front-end the browser gets — the one core serves at `/` — in a system webview. No third codebase (Vision decision 5).

The Tauri shell is `src/gui`, **its own cargo workspace**, not a host member. Not tidiness: Tauri adds **329 packages** not needed otherwise; as a member they'd land in `src/Cargo.lock` (406 → 735) and be resolved/built by every `cargo test`, `cargo clippy --workspace`, and CI run regardless of window changes. Kept separate, they're behind `make gui` only.

Cost measured before implementation ([#141]):

| | |
|---|---|
| Packages added, as measured then | **+256** (dedup vs. host; +329 today) |
| Clean release build | 332 CPU-seconds, 838 MB `target/` |
| Release binary | 9.6 MB (no bundled browser) |
| `deny.toml` entries | **11** (5 MPL-2.0, 6 unmaintained-advisory) |

All eleven are named crate-by-crate (not policy-widened) and structural: `wry` (Tauri's webview binding) depends on `dom_query` and `dirs`, so thinner alternatives pay the same licences. `deny.toml` carries reasoning and scope — dropping the shell drops that block.

Three consequences:

- **Default build path excludes it.** `make all` and `make gate` do not. `make gui` builds it and stages the binary beside `jan-klod` (found as a sibling, then `PATH`).
- **Supply-chain gates cover it.** `make lockfile`, `make deny`, `make audit` name all three workspaces; the widened tree doesn't escape.
- **Opt-in ship.** `make bundle GUI=1` adds it, suffixing `-gui`; `install.sh --gui` requests it. Client choice is orthogonal to distribution (second axis, not fourth) — see `scripts/distributions/README.md`.

On macOS the webview is WKWebView (no install needed). On Linux it is `webkit2gtk-4.1` (system package); `jan-klod --gui` names it on shell failure instead of falling back to terminal.

[#141]: https://github.com/PromptPasture/jan-klod/issues/141

#### The window is handed the token, and only on the core's origin

The web client keeps the gateway token in `sessionStorage` and prompts for 401 (`src/web/src/api.ts`). A window launched with a token shouldn't force a retype, so the shell seeds it via Tauri initialization script — making the window a credential boundary.

Two rules (both in `src/gui/src/main.rs`, both tested):

- The seed **checks `location.origin` first.** Initialization scripts run in every webview frame, so an unguarded one would hand the token to embedded pages.
- Navigation **off the core's origin is refused** and handed to the system browser (second-line guard).

`docs/concepts/security-model.md` details the checks and names the tests.

#### The web client's bundle is committed, and the alternative costs more than it looks

`src/web` builds to 5.5 kB ES module with no runtime dependencies; core embeds it via `include_str!` so the binary carries the page — no directory to ship. `include_str!` resolves at **compile time** — the bundle is either in the tree when `cargo build` runs or not; a decision, not a detail.

**Committed** for two reasons. First: the alternative is not "build in CI" — it is **Node becoming a core-build dependency**. Every `cargo build`, `make gate`, and CI job would need `npm run build` first; none install Node today. Large tax on everyone who never touches the web client so one generated file stays absent from git.

Second: this repo does this three times already. `src/protocol/schema/protocol.schema.json` is generated and committed with a drift test; `ext/*.manifest.toml` from real imports; `wit/wkg.lock` likewise. "No generated artifacts" isn't this repo's property, so preserving it here buys nothing while costing the first reason.

**What the choice costs.** Committed bundles can go stale: editing `src/web/src` and forgetting to rebuild serves the old page silently. `make web-dist-drift` catches it (#127): rebuilds into a temp tree and diffs (same regenerate-and-compare as the schema test). Works because esbuild is reproducible — macOS-built, Linux rebuilds byte-for-byte.

Unlike the schema's, it needs **Node**, so it's not on `make gate`; it runs in CI's lint-test job, not locally. Narrower guard: catches staleness on every push/PR, not at commit. That asymmetry is the real cost.

All UIs are clients of one surface, share one backend, carry no agent logic. Each holds no state the core lacks: every view is a session event log projection (Phase 14) delivered over the protocol (Phase 13).

## Agent loop architecture

The loop is a thin **conductor in core** running a fixed `stream → tools → loop` cycle. At **ordered phases**, it invokes enabled `interceptor-*` extensions natively (each exports `interceptor` interface) and acts on their `proceed | replace | block | ask` return. All *decisions* live in interceptors; the loop holds only *mechanism* (grammar passthrough, parse, validation, retry-with-correction, **provider fallback**, streaming handles, cancel, steering/follow-up queue). The phases, in order:

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

(Tokens stream live during `complete()` as a non-authoritative preview; `finalize` shapes the authoritative copy drivers reconcile against.)

Streamed tokens are **non-authoritative preview**; the loop emits the authoritative message at the boundary, which `after-response`/`finalize` may `replace` — drivers reconcile the two (Pi's `message_update → message_end` model). Mid-run **steering** and post-stop **follow-up** messages are driver-injected; tool results may set **`terminate`** to end the loop. Interceptors returning **`ask`** suspend the loop until the driver answers.

The **`on-error`** phase fires on non-recoverable errors (provider exhaustion, tool crash, validation failure after retries). An interceptor here may log, emit user-visible messages, or request retry — but the loop already exhausted internal retry logic, so `on-error` is for observability and graceful degradation, not error recovery.

See
[Small-Model Harness](small-model-harness.md) for how the mitigation strategies map
onto these hooks, and
[the decision record](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md)
for the rationale.

## Transport

The HTTP surface is **host-side** — `jan_klod_host::serve` runs a synchronous `tiny_http` listener exposing core over **REST + Server-Sent Events (SSE)**. UI clients, browsers, and remote ACP callers consume it. Curl-debuggable, browser-compatible, no stub generation. Endpoints: `GET /health`, `GET /sessions`, `POST /sessions`, `GET /session/:id`, `POST /session/:id/message`.

**It is no longer the only surface.** Phase 13 made the client surface a versioned contract ranked as WIT — `jan-klod-protocol`, with typed commands/notifications and version negotiated at connect (see [Contracts](contracts.md#ui--core-client-surface)) — and gave it a second transport:

**stdio JSON-RPC** (`jan-klod-gateway rpc`, `jan_klod_host::rpc`) is the TUI default and editor use. Newline-delimited JSON-RPC 2.0 on the process's stdin/stdout: **no port, no token, nothing left running** (client spawns and owns the gateway). Stdout carries frames only; all logs go to stderr.

The two differ structurally. Over REST, mid-turn confirmation answers on a *second connection* and cancel is client disconnect. Over stdio there's one pipe: a reader thread holds it during the turn, handing frames to the loop between turn events — how `turn/cancel` gets read and how `turn/follow-up` (steering a running turn) becomes possible, which REST cannot do.

**Phase 13c — WebSocket — is deferred until Phase 17 needs it** ([#43](https://github.com/PromptPasture/jan-klod/issues/43)). The obstacle is not framing but the socket: the protocol needs reads that time out (so silent clients cannot pin turns open — core is single-threaded, blocking the runtime, what `prompt_disconnect.rs` guards) and reads running while writes happen (so mid-turn `turn/cancel` is readable). `tiny_http`'s `Request::upgrade` returns two halves **fused**, with no `try_clone` or `set_read_timeout`, and private accessors aren't exposed. Paths out — a second listener or core-owned listener serving both — are best made by the client that will use one (not yet started). REST + SSE stays one contract projection, not a second contract. Vision [decision 1](../decisions/2026-09-08-harness-platform-vision/Vision.md#decisions); plan in [roadmap](roadmap.md#phase-13--client-protocol).

### The MCP port (`jan-klod-gateway mcp`)

A third surface, first one core doesn't define: **MCP's stdio transport is the framing `rpc` already speaks** — newline-delimited JSON-RPC 2.0, stdout frames and nothing else, stderr logs. `jan_klod_host::mcp` is a method-name-and-payload adapter over the same envelope, not a second transport, costing no dependency.

`rmcp`, the official Rust SDK, is async on tokio. This core is deliberately synchronous — the session is `!Send` and single-threaded (why `tiny_http` over `axum`) — so adopting it would be an architectural change.

Three tools: `ask` (one turn, answer as text), `session_list`, `session_get` (reading same payloads REST serves so editor and browser agree on session shape).

Two differences from `rpc` (both MCP's shape):

- **A frame may have no `id`.** `notifications/initialized` arrives after handshake with no answer. `rpc` refuses null id on purpose (uncorrelatable answer worse than none); carrying that rule across would reject every client's first message.
- **A failed tool is a *successful* response with `isError: true`.** Opposite of `protocol::jsonrpc`'s `Outcome` (result and error mutually exclusive, making "both" and "neither" unrepresentable). Mapping a refused turn to a JSON-RPC error would make every permission refusal read to editors as a broken server.

The driver is **headless by construction**: stdin carries protocol frames, so prompting would read a client's next request as the answer. Default answer is refusal, so the right policy and protocol behaviour come from one choice — editors couldn't answer anyway. `host/tests/it/mcp.rs::a_write_requiring_turn_is_refused_and_nothing_is_written` asserts the effect: the model asks to write a real path and the file isn't there.

**No security-model row, deliberately.** This surface grants nothing new — it re-exposes the existing turn path behind the same permission gate; the test above is evidence. A row asserting an unenforced boundary is worse than no row; recorded here so "no row" is a decision, not an omission.

### The ACP port (`jan-klod-gateway acp`)

The editor side, **the first surface where core is a JSON-RPC client and server on one pipe**. Everything else it serves is client→server requests + server→client notifications; ACP has the agent originate `session/request_permission` and block on the editor's answer.

Why this port needed `rpc`'s reader-thread shape and MCP didn't: during a turn, the serving thread is inside the conductor, so something else must hold the pipe or the answer won't arrive until after the turn it unblocks finishes.

Three differences from MCP beyond the direction:

- **Version is an integer** (`protocolVersion: 1`) vs. MCP's date string. Three schemes now coexist; none may copy another.
- **`session/new` mints the session id**, so clients must read it back before prompting. `acp::Connection` is frame-by-frame drivable, not just a read loop.
- **`stopReason` is not `isError`.** It says why a turn *ended*: `end_turn`, `max_tokens`, `max_turn_requests`, `refusal`, `cancelled`. Permission refusal is **`end_turn`** — `refusal` means the agent declined the whole exchange and the spec lets editors discard the user's prompt, so reporting a blocked tool call that way loses what the user typed. A genuinely failed turn has no stop reason and is a JSON-RPC error — opposite from MCP, where failure rides in-band.

**The editor's `cwd` is reported, not adopted.** `session/new` carries it; honouring it would let clients choose what file tools reach — workspace is a grant and `Workspace::open` already refuses `$HOME` and roots. Echoed in `_meta` so mismatches are visible.

**A disconnected editor needs no timeout.** Over REST answers arrive on a second connection, so vanished clients are invisible until a deadline. Here it arrives on the same pipe: the channel closes on EOF and the parked read returns at once (detection, not waiting). A closed pipe refuses (safe outcome).

## Command sandbox

`host-process` bounds the *caller* — default-deny, cwd jailed to workspace, timeout, output cap, scrubbed environment. Confining what the command itself *does* is separate per-OS.

**macOS: Seatbelt, deprecated but used.** `man sandbox-exec` opens with "DEPRECATED" and repeats it. Used anyway because it's what unprivileged processes get: no entitlement, no helper, no root. No unprivileged alternative exists; it's this or nothing (same conclusion Codex and Claude Code reached). Every command becomes `sandbox-exec -p <profile> -- <command>` with profile from `execution.sandbox`: `(deny default)`, reads allowed, writes only under `writable` paths, network only if policy allows.

**Linux: Landlock, no `unsafe`.** Landlock restricts *the calling process*, so obvious application is between `fork` and `exec` — `pre_exec`, an `unsafe fn` this workspace forbids. Instead the command rewrites to `<gateway> confine --writable <dir> [--network] -- <command>`, applying the ruleset **to itself** then `exec`ing the command. `exec` is safe, so the whole path is. The ruleset requires Landlock ABI 1's filesystem rights as hard requirement (no confinement without them), newer rights best-effort; denying network is hard requirement (needs ABI 4, kernel 6.7), so kernels lacking it refuse the command. Anything short of `FullyEnforced` refuses.

Both backends share a shape: **both replace the program with a wrapper that confines then becomes the command.** That's why `SandboxBackend::confine` takes and returns an owned `Command` rather than borrowing — a `Command`'s program can be read but not changed.

**The fallback is `approval-only`, stated not assumed.** A platform with no backend or macOS without `/usr/bin/sandbox-exec` resolves to approval-only; the boot line says which — the runtime doesn't claim unachieved confinement. Operators wanting no unconfined commands set `require: true`, denying `host-process` outright instead of degrading. Grants and tests are the "Command effects" row in the [security model](security-model.md#capabilities).

**A long-lived child goes through exactly this.** A stdio MCP server, `lsp-*`, or browser driver must be *held open*, which `host-process`'s `exec` cannot do (runs to completion). `spawn` holds one, reaching the process through the same `ProcessRunner::prepared`: confine, then cwd, environment, pipes, start. A child outliving its call is more exposed, so it gets the same policy, not a relaxed one; the single place deciding command bounds applies to both.

What differs is grant and lifetime. The grant, `execution.long-lived`, **names processes** rather than permitting spawning: guests ask for a name and the host supplies the command, so only operator-specified commands run. Lifetime is the host's: the child is owned by the extension that started it and killed when that instance exits (including on gateway exit), guest request or not. Guests forgetting or trapping before asking leave nothing behind.

Two things Seatbelt deliberately doesn't do: **reads are not confined** (effects are the gap being closed; commands that can't read toolchain don't run), and commands needing **Mach services** fail rather than run unconfined — visible in the command's own error, the right failure direction.

## Storage

| Backend | Status | Notes |
|---|---|---|
| SQLite | **Shipping** | Zero-ops; `rusqlite` bundled, host-side (not SQLite-in-wasm) |
| PostgreSQL | Not built | Second host backend behind the same `Store` type |

The persistent store is a **host-side capability** core exposes through `host-storage` — it is *not* SQLite-in-wasm (Go MVP proved it doesn't work). Configured by the top-level `storage:` block: `path` for durable file, absent for in-memory.

Deliberately **not an extension**; the `store-*` component family never existed. Sandboxes have no filesystem, so store guests would need one granted back; conversation transcripts are the runtime's most sensitive asset, so fewer parties should hold them. Swappable backend components would trade the one guarantee this design makes for a plugin nobody asked for. Postgres/Supabase arrive as host backends behind the same `Store` type if they come.

### Compile cache

Configured in the same `storage:` block: `cache-dir` (default `wasmtime-cache`, beside `config.yaml`) is where Wasmtime caches compiled components ([#60](https://github.com/PromptPasture/jan-klod/issues/60)), so second boots skip Cranelift recompilation. Wasmtime's own built-in cache (`Cache`/`CacheConfig`, `Config::cache`) wired at `Runtime::boot` — not a bespoke `.cwasm` cache. Evaluated first per the issue and adopted because it keys on everything affecting codegen (component bytes, target triple, compiler/ISA flags, Wasmtime version) and treats corrupt/foreign artefacts as misses, never errors. See `core/src/wasm_cache.rs` for evaluation and [Security model](security-model.md) for what cache hits trust.

### Two tables

`entries` is the key/value store `host-storage` serves: `(namespace, key)` with opaque JSON values, updated in place. `events` is the **append-only turn log**:

```sql
events(session TEXT, seq INTEGER, ts INTEGER, kind TEXT, payload TEXT,
       PRIMARY KEY (session, seq))
```

One row per event, `seq` numbered from 1 per session — a log reads as a gapless sequence. `seq` is allocated inside the insert (`SELECT COALESCE(MAX(seq), 0) + 1 … RETURNING seq`), preventing collisions. No update and no single-row delete: logs whose rows could be rewritten make replays claims about the present, not the past. `purge_session_events` (forgetting a whole session) is the only removal.

What's logged and by what: every `conductor::Event` via `PersistingSink` (fan-out, so SSE stream and TUI receive everything); the turn-starting message; `ask`, answer, and steering follow-up via `PersistingDriver`. Answers record regardless of provenance, including defaults when nobody replied (replays can't otherwise distinguish approvals from timed-out denials). Text deltas store one row per event, not coalesced — replaying clients need them as they arrived.

Logging is best-effort: store failures report once per turn, never cancelling it — a full disk shouldn't stop conversations.

**The log is the only session record.** `entries` held `{user, answer}` rows per turn until Phase 14b; nothing writes them now. A second session copy in a second format is what that phase removed.

### The envelope, and why it has its own version

Every `payload` is `{"v": <EVENT_LOG_VERSION>, "data": {…}}`. `EVENT_LOG_VERSION` (`core::event_log`) is **not** the client protocol version (merging would be a mistake): stored logs outlive watching clients, so "can this build read this row?" and "can this client talk to this core?" are different questions with different answers. Readers refuse newer versions rather than guess, and accept older ones.

### Everything else is a projection of it

`core::projection::transcript` turns rows into the `Vec<Message>` a turn replays — a pure function, no store/I/O. Each row places the way `conductor::run_turn` places it, not by projection rules: a `follow-up` is a user message (how steering injects), a `tool-result` is a `Role::Tool` message with the call's id (how the loop feeds results back). `text-delta` rows drop in favour of `done` (conductor's own words: deltas are non-authoritative preview), so agentic turns' intermediate assistant texts aren't in the transcript — same as the `{user, answer}` transcript this replaces.

`ask` and answers drop too (load-bearing decision): the prompt goes to the *user* by an interceptor over a channel the model doesn't touch, so rendering the question as an assistant message puts words in the model's mouth, and the answer as a user message makes a permission click look user-typed.

Resume reads the projection bounded to the last `REPLAYED_TURNS` turns, counted over `user-message` rows — turn boundaries, not row counts, because turns have variable rows and row bounds would open with a tool result answering an unseen call. `GET /session/:id` serves the projection as `messages`, `GET /sessions` previews it, and `POST /session/:id/fork` copies a prefix into a new session that diverges, renumbered from 1 with no link back.

### Upgrading a database written before the log

A one-shot conversion runs at boot (`event_log::migrate_transcripts`, from `Runtime::open_store`), turning each old `turn-N` row into `user-message` and `done`, dated with the row's own timestamp so old sessions aren't re-dated to upgrade. Idempotent — sessions with any events are skipped — and old rows stay (deleting them makes conversion unrepeatable/irreversible).

Required, not optional: with log-based read surfaces, unconverted databases have unlisted, unreadable sessions. Unrecoverable is what the old format never held — tool calls, warnings, `ask` and answer — so migrated turns are exactly two events, with `done` carrying `agentic: false` because the transcript didn't record them.

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

When a provider/model fails (unavailable, rate-limited, quota exceeded, OOM), the **core loop** falls back through a two-level priority list in `config.yaml`, re-issuing to the next entry. Fallback is core *mechanism*, not an interceptor: it re-issues the *same* failed request (on-provider-error retry, same category as retry/validate), which `prepare-next-turn` interceptors cannot do. Entries reference **provider instance names** (`extensions.provider.<name>`), not wasm components — see [Configuration](configuration.md):

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

Fallback order: try each model in the current provider → next provider → all exhausted, surface error (no silent retry spiral).

Fallback is per-request — if primary recovers, next request uses it. Enables cost routing: cheap tasks naturally use smaller/cheaper models without separate config.

## Model catalog

Loop decisions need facts *about models* the models don't report: context-window, modalities, reasoning support, token pricing. Jan-Klod keeps a small **per-model catalog** — model id to metadata lookup:

| Field | Used by | For |
|---|---|---|
| `context-window` | `interceptor-context` (`select-context`) | budget to trim/compress history |
| `max-output` | core loop | capping `max-tokens` |
| `modalities` (`text`, `image`, …) | `interceptor-task-router` (`select-model`) | routing to capable models |
| `reasoning` | `interceptor-task-router` | routing reasoning-heavy tasks to capable models |
| `cost` (input / output / cache) | `interceptor-task-router` | cost-aware routing — cheap tasks to cheap models |

The catalog is **data, not policy** — a refreshable table shipped with defaults, overridable in `config.yaml`, read by request-shaping interceptors via `host-config`. It lets `select-context` size budgets and `select-model` route by capability/cost without hard-coding model facts. New models extend the table; no code change.

## Task routing

`interceptor-task-router` classifies requests into a task type and routes to the configured provider/model (sets model at `select-model` phase). Jan-Klod ships built-in task types as defaults; users extend or override in `config.yaml`.

**Built-in task types:**

| Task type | Use |
|---|---|
| `code-generation` | Generate code |
| `code-review` | Review code |
| `file-edit` | Edit files |
| `web-search` | Search web |
| `research` | Multi-step information gathering |
| `reasoning` | Complex analysis |
| `planning` | Break goals into steps |
| `chat` | General conversation |
| `clarification` | Resolve ambiguity |
| `agent-delegation` | Delegate via ACP |

User-defined types in `config.yaml` — the LLM classifier receives all types and picks the closest match. No code changes to add types.

Routing: `<provider-instance>/<model>`, instance names under `extensions.provider`:

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

> **Deferred (not in v1 interceptor set).** Decomposition fans out independent sub-runs and merges them — doesn't fit `proceed`/`replace`/`block`/`ask` shape, a later addition (likely `interceptor-decomposer` driving child loops or driver-side concern). Recorded here as intent, not Phase 2 commitment. See [open questions](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md#open-questions).

For tasks with independent subtasks, a decomposition interceptor splits work and dispatches in parallel:

- **`file-edit`** — each file in parallel, results merged
- **`web-search`** — multiple queries in parallel, merged before LLM synthesis
- **`research`** — multiple sources fetched in parallel
- **`code-review`** — each module independently

Single-subtask requests skip decomposition.

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

`registry-mcp` monitors connected MCP servers. On crash/disconnect:

1. Mark server as `down`.
2. Remove its tools from the active set — `interceptor-tool-selector` won't offer them at `select-tools`.
3. Emit bus event — UI clients warn the user.
4. Reconnect on exponential backoff.

No crash propagates to core; the agent loop continues with remaining tools.

## Config hot-reload

Extensions pick up `config.yaml` changes without restart. Core watches the config and notifies affected extensions via the event bus. Extensions opt in by implementing the reload lifecycle hook.

## Deployment targets

| Target | Notes |
|---|---|
| Desktop (macOS, Windows, Linux) | Primary; all UI modes |
| ARM home server / NAS | Low memory (Rust + WASM); headless core, no UI — e.g., `chat-telegram` for access, optionally `api-rest` |
| Docker | Single container; config via env vars or mounted `config.yaml` |
| Kubernetes | Enterprise; stateless API scaling needs shared Postgres (not built) |

## Deployment modes

- **Standard:** `core` binary + `ext/*.wasm` + `config.yaml` (deploy unit). UI binary is separate, optional.
- **Bundle:** pre-packaged ZIP with core + curated `.wasm` + pre-filled config; UI bundles include the UI client.

See [Configurator](configurator.md) for archive generation and [Blue/Green Deployment](blue-green-deployment.md) for updates.
