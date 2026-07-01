# Phase 2 Plan — First Real Value: the Agent Loop

> **Re-architected 2026-07-01** around
> [Thin Loop + Interceptor Middleware](../2026-07-01-thin-loop-interceptors/BRAINSTORM.md).
> The loop *mechanism* now lives in **core** (Rust); every *decision* is a sandboxed
> **`interceptor-*`** extension the core loop calls natively at an ordered list of
> **phases** (`wit/interceptor.wit`). The monolithic `manager-agent-loop` /
> `agent-manager-world` / `manager-context` model is **retired**. Only the Slice 2a
> **router logic** carries forward — recast as `interceptor-intent-router`.

Living execution checklist for Phase 2 of the
[Roadmap](../../concepts/roadmap.md). Update the flags here and in the roadmap
[Status tracker](../../concepts/roadmap.md#status-tracker) as work proceeds.

Prerequisite: Phase 1 exit gate passed (2026-07-01).
Relevant background: [small-model-harness.md](../../concepts/small-model-harness.md) ·
[architecture.md](../../concepts/architecture.md#agent-loop-architecture) ·
[contracts.md](../../concepts/contracts.md) · [`wit/interceptor.wit`](../../../wit/interceptor.wit).

## Goal

The runtime does something useful end-to-end: a user query enters the **core loop**,
`interceptor-intent-router` decides simple-vs-agentic, the request-shaping
interceptors (`select-model` → `select-context` → `select-tools`) assemble the
completion, the core drives a ReAct cycle with grammar-constrained decoding +
retry/validate, provider fallback resolves the right model on failure,
`interceptor-permission` gates tool calls (and can `ask` the driver), and the loop
returns a grounded answer.

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Item | Flag |
|---|---|
| Slice 2a — Intent router logic (recast into `interceptor-intent-router`) | `done` |
| Slice 2b — `interceptor` contract + core-native dispatch framework | `done` |
| Slice 2c — Core loop mechanism (conductor, harness, fallback, run entry) | `done` |
| Slice 2d — v1 interceptor set (task-router, context, tool-selector, permission) | `done` |
| Slice 2e — Exit gate | `done` |

## Architecture context

The loop is **core mechanism**, not an extension. The core exposes a **loop entry**
(a `run-handle`, same poll pattern as `llm-provider`) that a driver calls; in Phase 2
the driver is the offline test harness (the WIT *driver capability* that `api-*`/
`chat-*` will import is deferred to Phase 3/4 when inbound network lands).

Interceptors are sandboxed guests exporting the one generic `interceptor` interface
([`wit/interceptor.wit`](../../../wit/interceptor.wit)):

```wit
intercept:         func(input: intercept-input) -> result<decision, interceptor-error>
subscribed-phases: func() -> list<phase>
// decision = proceed | replace(hook-state) | block(block-reason) | ask(user-prompt)
// phase    = session-start | before-loop | select-model | select-context
//          | select-tools | after-response | tool-call | tool-result
//          | on-error | finalize | prepare-next-turn
```

**Dispatch is core-native and ordering is structural:** the core calls each enabled
interceptor's `intercept` at the phases it lists in `subscribed-phases()`, across
phases in enum order and within a phase in deterministic load order. `config.yaml`
**only enables/disables** interceptors — it never orders them.

**Retirements (cleanup tasks in the slices below):** `manager-agent-loop` (guest),
`agent-manager.wit`, `agent-loop.wit` (v0 stub), `context-manager.wit` — their
routing/history/fallback responsibilities move to core mechanism or to interceptors.

---

## Slice 2a — Intent router logic → `interceptor-intent-router`

**GitHub:** #24

The layered router (`whatlang` language detection → heuristic tier → LLM classifier)
is **built and unit-tested** (17 offline tests), currently living inside the
`manager-agent-loop` guest. What remains is **repackaging it as an interceptor
extension** at the `before-loop` phase.

Carried forward (done):
- [x] **Language detection** — `whatlang` 0.16, pure-Rust, no model call.
- [x] **Heuristic tier** — ~90 rules across seven named `const` groups; emits
  `simple` or `pass`.
- [x] **LLM classifier tier** — single constrained-decoding call
  (`grammar = "root ::= \"simple\" | \"agentic\""`); ambiguous/failed → `agentic`
  (never wrongly skip the loop).

Recast:
- [x] **New guest `src/extensions/interceptor-intent-router/`** exporting
  `interceptor-world`; `router` module moved in verbatim (17 tests pass unchanged).
- [x] **`subscribed-phases()` → `[before-loop]`**; `intercept` implemented for the
  `before-loop(user-turn)` case: `simple` → `block`, `agentic` → `proceed`. (The
  `block-reason` currently carries only a message; wiring a direct-answer payload is
  deferred to the core loop, Slice 2c.)
- [x] **LLM classifier via the routed `llm-provider` import** (available on
  `interceptor-world`), replacing the in-manager provider call.
- [x] **Build** — `make interceptor-intent-router[-docker]`; the staged component
  exports `interceptor` + `extension-lifecycle`.
- Fixed two latent `interceptor.wit` bugs surfaced by building the world (the
  Slice 2b "finalize the contract" item): the `error-context` record collided with a
  reserved WIT keyword (renamed `error-info`) and its `failed-state:
  option<hook-state>` field made `hook-state` self-referential (removed;
  `on-error` is observation-only in v1). `wasm-tools component wit wit/` now passes.
- [x] **Core dispatch integration** — done via the Slice 2b wasm adapter: the guest
  loads through `interceptor_host::WasmInterceptor` and is dispatched at `before-loop`
  by the `Dispatcher` (see the 4 adapter integration tests).

**Definition of done:** the guest loads through the core, is dispatched only at
`before-loop`, classifies greetings/acks as `simple` (no model call) and multi-step
prompts as `agentic`; the existing 17 router unit tests pass unchanged in the new
crate. *(Router recast + unit tests done; core-dispatch half blocked on Slice 2b.)*

---

## Slice 2b — `interceptor` contract + core-native dispatch framework

**GitHub:** #25 · **Blocked on:** —

Finalize the contract and build the host machinery that drives it. This is the
foundation both the core loop (2c) and every interceptor (2a, 2d) hang off.

- [x] **Finalize [`wit/interceptor.wit`](../../../wit/interceptor.wit)** — validates
  via `wasm-tools component wit wit/` (fixed the `error-context` keyword clash and
  the self-referential `hook-state` in Slice 2a); `hook-state` records confirmed
  against the adapter mapping. The `run-handle`/driver **loop-entry** interface is
  Slice 2c's concern (prototyped there as a core Rust entry, not yet a host cap).
- [x] **`bindgen!` the interceptor world** in core — `interceptor_host::WasmInterceptor`
  instantiates a guest, satisfies all five world imports (`host-log`/`host-config`/
  `host-event` + in-memory `host-storage` + an injected canned `llm-provider`),
  resolves `subscribed-phases()` at boot, and implements the `Interceptor` trait via
  full host↔generated type mapping. 4 integration tests drive the real
  `interceptor-intent-router.wasm` through the `Dispatcher` (heuristic block with no
  provider call; LLM tier via the canned provider; provider verdict honoured).
- [x] **Dispatch engine** (`jan_klod_core::intercept`): given a phase and a mutable
  loop state, call each enabled+subscribed interceptor in order, apply its
  `decision` — `proceed` (no-op), `replace` (swap the phase state), `block`
  (short-circuit with reason), `ask` (suspend → `Driver::ask` → resume the same
  interceptor with `answer` set). Built Wasmtime-decoupled behind an `Interceptor`
  trait so it is unit-tested with stubs (6 tests); the wasm-guest adapter is one
  implementor, landing with the `bindgen!` item below.
- [x] **Error/trap policy** — on `Err` (a trap surfaces as `Err` through the
  adapter), **fail closed at `tool-call`** (treat as `block`), fail-open-with-log
  elsewhere. *(Event-bus emission of the offending id is wired with the adapter.)*
- [x] **Registration (enable/disable)** — interceptors are a generic extension
  category, so `config.yaml` enable/disable already flows through
  `jan_klod_config` with no code change: `interceptor.intent-router` resolves to
  `interceptor-intent-router.wasm` (verified via the config dump). `config.yaml`
  gained an `interceptor:` section (intent-router enabled; the four Slice 2d
  interceptors disabled) and dropped the retired `manager.context`. *(Resolving
  `subscribed-phases()` at boot + deterministic load order land with the wasm
  adapter.)*
- [x] **`configuration.md` note** — added an "Interceptors" subsection: enable/disable
  only, ordering is structural (phase order + load order), never configured.
- [x] **Retire** `context-manager.wit` and `agent-manager.wit` (removed — unused by
  any build; `agent-manager` was the only consumer of `context-manager`). README +
  `types.wit` doc-comment updated. `agent-loop.wit` stays until Slice 2c retires the
  `manager-agent-loop` guest and its `route.rs` path. `wasm-tools component wit wit/`
  stays green.

**Definition of done:** an offline test registers two stub interceptors on the same
phase and one on another, and asserts: order (across + within phase), each decision
variant is applied, a `tool-call` trap fails closed, and an `ask` round-trips through
a canned driver. `wasm-tools component wit wit/` stays green.

---

## Slice 2c — Core loop mechanism

**GitHub:** #26 · **Blocked on:** Slice 2b (#25)

Build the thin conductor in core. Zero policy — all decisions are delegated to the
dispatch engine from 2b.

- [~] **Loop entry** — `Runtime::build_agent(&http_factory)` boots the enabled
  `interceptor.*` as a `Dispatcher` + the enabled `provider.*` as a `ProviderCompleter`
  fallback chain; `AgentSession::run(session, message)` drives the conductor
  end-to-end (headless `Driver`). Verified by `host/tests/agent_loop.rs`: from a real
  `config.yaml`, a greeting short-circuits (`agentic:false`) and a multi-step prompt
  runs the agentic path (`agentic:true`), both through the sandboxed provider +
  interceptor; `AgentSession::run_with` exposes the driver + `ToolInvoker` seams for
  the integrated ReAct/permission gate. *Carried forward (post-v1, not gate-blocking):*
  the streaming `run-handle` (`next-event` / `cancel` / `provide-answer` / steering
  queue), and routing an interceptor's `llm-provider` to the real providers (v1 uses a
  safe-default classifier).
- [~] **Conductor** (`jan_klod_core::conductor`) — `before-loop` (short-circuits a
  simple prompt) → `select-model` → `select-context` → `select-tools` (assembling
  `pending-request`) → the **ReAct loop**: `complete()` → `after-response` → parse
  tool calls → `tool-call` gate → tool dispatch (via the `ToolInvoker` seam,
  skip-if-absent) → `tool-result` (a block here terminates the loop) → repeat (capped
  at 8 iterations) → `finalize`. Trait-decoupled from Wasmtime, unit-tested with
  stubs (8 tests incl. a ≥2-cycle ReAct run, permission-deny, and terminate).
  *Still to add:* `session-start`, `prepare-next-turn`, and the wasm run-handle entry.
- [~] **Small-model harness (core mechanism)** — **parse + structural validation**
  (`validate`: every tool call's `arguments` must be valid JSON) and
  **retry-with-correction** on malformed output (`complete_validated`: feed the bad
  output back with a correction and re-issue, default N=3, no silent spiral) are
  built and unit-tested (2 tests). *Still to add:* passing the `grammar` on the
  request + default grammar **construction** from the active tool set (lands with the
  wasm run entry, where the real `completion-request` is assembled).
- [x] **Provider fallback (core mechanism)** — `complete_with_fallback` re-issues the
  *same* request down the `Completer` chain; first success wins, exhaustion returns a
  diagnostic `Failed`. `route::ProviderCompleter` adapts a routed provider extension
  to the `Completer` trait (drains the stream into text + tool calls), verified
  against the real `provider-openai.wasm` with canned http. *(A `warning` event on
  fallback lands with the run entry + event bus.)*
- [ ] **Streaming** — preview-vs-authoritative: stream tokens live during each
  `complete()`; emit the authoritative message at the turn boundary (which
  `after-response`/`finalize` may have `replace`d).
- [x] **Tool dispatch** — the conductor routes each parsed tool call through the
  `tool-call` gate then the `ToolInvoker` seam (skip-if-absent → the model is told
  "no tool named …"); a `tool-result` block is the `terminate` signal. *(Wiring the
  seam to the routed `tool-callable` extensions lands with the wasm run entry.)*
- [x] **Retire the `manager-agent-loop` guest** and the `agent-loop-world` stub path.
  Removed: the guest crate, `wit/agent-loop.wit`, the v0 `route::build_routed_loop` /
  `RoutedAgentLoop` / `ManagerHost` / `store-world`+`agent-loop-world` bindings and
  their `route_agent_loop` entry, `host/tests/routing.rs`, the `config.yaml` `manager:`
  section, and the Makefile/README entries. `route.rs` now holds only the provider
  machinery (`CapHost` + `ProviderCompleter`). `wasm-tools component wit wit/` green;
  full core suite green.

**Definition of done:** a multi-step prompt against a canned `host-http` drives ≥2
ReAct cycles; malformed output triggers retry+correction; a simulated `rate-limited`
engages fallback to a second provider; `next-event` streams correctly; verified by
`make harness`.

---

## Slice 2d — v1 interceptor set

**GitHub:** #27 · **Blocked on:** Slice 2b (#25); integration needs 2c (#26)

The remaining four interceptors (intent-router is Slice 2a). Each a sandboxed Rust
guest exporting `interceptor-world`. Two are **built thin** (real but single-rule) to
prove the seam without speculative machinery.

- [x] **`interceptor-task-router`** (`select-model`) — classifies the request into a
  built-in task type via a constrained `llm-provider` call, resolves the `routing:`
  entry (`routing.<task>` → `provider/model`, served through `host-config`) and sets
  `pending-request.model`; proceeds (model unset) when classification or routing
  yields nothing. Built + staged; 3 native tests + 3 adapter tests (subscribes to
  `select-model` only; sets the model from the table; proceeds with no route).
  *(User-defined task types are a later refinement.)*
- [x] **`interceptor-context`** (`select-context`) — trims the assembled history to
  the model budget: char/4 estimate + sliding window (keep system + most recent, drop
  oldest, always keep the current turn); budget from `host-config` `context-tokens`
  (default 8192). Summarisation via `llm-provider` is deferred behind the same seam.
  Replaces `manager-context` + `context-manager.wit`. Built + staged; 4 native trim
  tests + 3 adapter tests (subscribes to `select-context` only; trims over-budget
  history; leaves small history untouched).
- [x] **`interceptor-tool-selector`** (`select-tools`) — **thin**: a working
  pass-through that exposes the assembled tool set and proceeds (tool sources
  `mcp-registry`/`tool-callable` are wired later; per-step narrowing is a later
  refinement). Built + staged; 2 adapter tests (subscribes to `select-tools` only;
  passes through).
- [x] **`interceptor-permission`** (`tool-call`) — **thin**: a single-rule gate that
  flags a dangerous tool by name (`rules::is_dangerous`), returns `ask` to confirm,
  and `proceed`/`block`s on the answer (`rules::is_affirmative`). Built + staged;
  3 native rule tests + 4 adapter tests driving the real guest through the
  `Dispatcher` (subscribes to `tool-call` only; deny→block; approve→proceed; ordinary
  tool never asks). Exercises the `ask` round-trip end-to-end.
- [x] **Build targets** — all four wired into the extensions Makefile
  (`make interceptor-<name>[-docker]`), added to `GUESTS` (+ `TESTABLE_GUESTS` for the
  three with native logic).
- [x] **Harness tests** — each guest has core adapter tests (lifecycle via
  `instantiate` + `subscribed-phases` + an `intercept` round-trip through the real
  component), skipped when the component is not staged.

**Definition of done:** all four load through the core, are dispatched only at their
phases, and drive their decisions end-to-end in the harness (task-router sets a model;
context trims a synthetic long history; tool-selector passes tools through; permission
asks then blocks/allows).

---

## Slice 2e — Exit gate

**GitHub:** #28 · **Blocked on:** all of #24–#27

- [x] Offline test `tests/phase2_gate.rs` — canned `host-http`, no network / API key.
  Boots the real `Runtime` from a `config.yaml` with **two provider instances**, a
  `routing:` section, and **all five v1 interceptors enabled**; calls the loop entry
  (`build_agent` → `AgentSession::run`) with a multi-step query and a greeting.
  Asserts the wired path end-to-end across **two tests**: (1) the intent router fires at
  `before-loop` (greeting → simple short-circuit `agentic:false`; multi-step →
  `agentic:true`); the shaping interceptors run (task-router resolves `routing.chat`,
  context, tool-selector); the primary provider's transport fails so **provider
  fallback** engages and the secondary answers with a grounded answer. (2) The
  **integrated ReAct + permission path**: a provider emits a tool call then a final
  answer; the real `interceptor-permission` guest `ask`s, the driver approves, the
  canned tool runs (`AgentSession::run_with` wires the driver + `ToolInvoker` seams),
  the result feeds back, and the loop returns the answer. *(Retry-with-correction stays
  proven at the unit level in the conductor tests.)*
- [x] CI — `make phase2-gate` added to `.github/workflows/ci.yml` (harness job); runs
  offline. `make harness` also updated (the retired `routing` test → the new
  `agent_loop` test).
- [x] **Mark Phase 2 `done`** — the roadmap exit gate (intent → shaped request → ReAct
  cycle → fallback on a simulated failure → grounded answer, via the loop entry) passes
  end-to-end through the sandboxed guests. Marked `done` here and in
  [roadmap.md](../../concepts/roadmap.md). Carried-forward refinements (not gate-blocking):
  the streaming `run-handle` (`next-event`/steering), routing an interceptor's
  `llm-provider` to the real providers (v1 uses a safe-default classifier), and the
  `tool-callable` fleet that populates the tool set — all Phase 3+/roadmap items.

**Definition of done:** `make phase2-gate` passes in CI (green); `roadmap.md` status
tracker updated to `done`. ✓

---

## Open questions feeding Phase 2

Not blockers, but resolve before the slice that first needs each.

| Question | First needed | Current lean |
|---|---|---|
| `hook-state` payload completeness (does `select-context` need an explicit token-budget field, or infer from `model` + `host-config`?) | Slice 2b | Infer budget from the [model catalog](../../concepts/architecture.md#model-catalog) (`context-window`/capabilities/cost) via `host-config`; add a `hook-state` field only if a v1 interceptor needs it |
| Core-exposed **loop-entry / driver capability** WIT shape (run/next-event/ask/steering) | Slice 2b | Prototype as a core Rust entry in Phase 2; promote to a host capability when `api-*` lands (Phase 3/4) |
| Constrained-decoding grammar format (GBNF vs JSON-schema) | Slice 2c | GBNF (llama.cpp); confirm vLLM / Ollama accept the same `grammar` field |
| Intra-phase load order determinism (multiple interceptors on one phase) | Slice 2b | Registry insertion order; document it — config never sequences |
| Token-count estimate strategy for `interceptor-context` v1 | Slice 2d | char/4 heuristic; tiktoken-rs if accuracy matters |
| Whether classification interceptors take the `llm-provider` import or the loop pre-classifies | Slice 2d | Take the import (already on `interceptor-world`); revisit if double-classification is wasteful |
| Retry limit N | Slice 2c | 3; `host-config`-overridable |
| Optional imports (`skill-registry`, `mcp-registry`, `tool-callable`, `agent-delegate`) | Slice 2c/2d | Skip-if-absent; stub linker entries if Wasmtime requires them |

## Cross-cutting (continuous)

- Strict clippy lint policy (`[workspace.lints]` inherited or declared locally) on
  every new crate — same policy as Phase 1.
- `cargo test` stays green on every PR; `make harness` covers component-level tests;
  `make phase2-gate` is the integration gate.
- Supply-chain gates (`cargo-audit`, `cargo-deny`, `govulncheck`) extended to cover
  new guest crates.
- Structured logging: every new component tags its `host-log` lines with its name
  (e.g. `[interceptor.intent-router]`, `[interceptor.context]`).
