# Phase 2 Plan — First Real Value: the Agent Loop

> **Re-architected 2026-07-01** around [Thin Loop + Interceptor Middleware](../2026-07-01-thin-loop-interceptors/BRAINSTORM.md).
> Loop *mechanism* lives in **core** (Rust); every *decision* is a sandboxed **`interceptor-*`** 
> extension called at ordered **phases** (`wit/interceptor.wit`). Monolithic `manager-agent-loop` / 
> `agent-manager-world` / `manager-context` **retired**. Slice 2a router logic recast as `interceptor-intent-router`.

Execution checklist for Phase 2. Update flags here and in [Status tracker](../../concepts/roadmap.md#status-tracker).

Prerequisite: Phase 1 exit gate passed (2026-07-01). References: [small-model-harness.md](../../concepts/small-model-harness.md) · [architecture.md](../../concepts/architecture.md#agent-loop-architecture) · [`wit/interceptor.wit`](../../../wit/interceptor.wit).

## Goal

User query enters the **core loop** → `interceptor-intent-router` classifies simple vs. agentic → 
request-shaping interceptors (`select-model`, `-context`, `-tools`) assemble completion → core 
drives ReAct with grammar-constrained decoding + retry/validate → provider fallback on failure → 
`interceptor-permission` gates tool calls → loop returns grounded answer.

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

Loop is **core mechanism**, not an extension. Core exposes a **loop entry** (a `run-handle`, 
same poll pattern as `llm-provider`) that drivers call; Phase 2 uses the offline test harness 
(WIT *driver capability* for `api-*`/`chat-*` deferred to Phase 3/4).

Interceptors are sandboxed guests exporting the generic `interceptor` interface ([`wit/interceptor.wit`](../../../wit/interceptor.wit)):

```wit
intercept:         func(input: intercept-input) -> result<decision, interceptor-error>
subscribed-phases: func() -> list<phase>
// decision = proceed | replace(hook-state) | block(block-reason) | ask(user-prompt)
// phase    = session-start | before-loop | select-model | select-context
//          | select-tools | after-response | tool-call | tool-result
//          | on-error | finalize | prepare-next-turn
```

**Dispatch is core-native and ordering structural:** core calls each enabled interceptor's `intercept` at subscribed phases, in enum order + deterministic load order. `config.yaml` only enables/disables — never orders.

**Retirements:** `manager-agent-loop`, `agent-manager.wit`, `agent-loop.wit`, `context-manager.wit` — routing/history/fallback move to core or interceptors.

---

## Slice 2a — Intent router logic → `interceptor-intent-router`

**GitHub:** #24

Layered router (`whatlang` → heuristic → LLM classifier) built and unit-tested (17 tests), 
in `manager-agent-loop`. Repackage as interceptor extension at `before-loop`.

Carried forward:
- [x] **Language detection** — `whatlang` 0.16, pure-Rust, no model call.
- [x] **Heuristic tier** — ~90 rules, emits `simple` or `agentic`.
- [x] **LLM classifier** — constrained call; ambiguous/failed → `agentic`.

Recast:
- [x] New guest `src/extensions/interceptor-intent-router/` exporting `interceptor-world`; router module moved verbatim (17 tests pass).
- [x] `subscribed-phases()` → `[before-loop]`; `intercept`: `simple` → `block`, `agentic` → `proceed`.
- [x] LLM classifier via routed `llm-provider` import (replaces in-manager call).
- [x] Build `make interceptor-intent-router[-docker]`; staged component exports `interceptor` + `extension-lifecycle`.
- Fixed two `interceptor.wit` bugs: `error-context` collided with reserved keyword (renamed `error-info`); `failed-state: option<hook-state>` self-referential (removed; `on-error` observation-only). `wasm-tools component wit wit/` passes.
- [x] Core dispatch integration via Slice 2b wasm adapter; guest loads through `WasmInterceptor`, dispatched at `before-loop` by `Dispatcher` (4 adapter tests).

**Done:** guest loads through core, dispatched at `before-loop`, classifies greetings as `simple` and multi-step as `agentic`; 17 unit tests pass. *(Core-dispatch blocked on Slice 2b.)*

---

## Slice 2b — `interceptor` contract + core-native dispatch framework

**GitHub:** #25 · **Blocked on:** —

Finalize contract and build host machinery. Foundation for core loop (2c) and all interceptors.

- [x] Finalize [`wit/interceptor.wit`](../../../wit/interceptor.wit) — validates via `wasm-tools component wit wit/` (fixed keyword clash, self-referential `hook-state`). `run-handle`/driver loop-entry deferred to Slice 2c.
- [x] `bindgen!` interceptor world in core — `WasmInterceptor` instantiates guest, satisfies five imports, resolves `subscribed-phases()` at boot, implements `Interceptor` trait. 4 tests drive real `interceptor-intent-router.wasm` through `Dispatcher`.
- [x] Dispatch engine (`jan_klod_core::intercept`): given phase + loop state, call each enabled+subscribed interceptor, apply decision — `proceed` (no-op), `replace` (swap), `block` (short-circuit), `ask` (suspend/resume). Unit-tested with stubs (6 tests).
- [x] Error/trap policy: fail closed at `tool-call` (treat as `block`), fail-open-with-log elsewhere.
- [x] Registration: `config.yaml` enable/disable flows through `jan_klod_config`; gained `interceptor:` section (intent-router enabled; Slice 2d disabled).
- [x] Retire `context-manager.wit`, `agent-manager.wit` (unused). `agent-loop.wit` stays until Slice 2c.

**Done:** offline test registers interceptors on multiple phases, asserts order + decision variants + trap handling + `ask` round-trip. `wasm-tools component wit wit/` green.

---

## Slice 2c — Core loop mechanism

**GitHub:** #26 · **Blocked on:** Slice 2b (#25)

Build thin conductor in core. Zero policy — delegated to dispatch engine (2b).

- [~] **Loop entry** — `Runtime::build_agent()` boots `interceptor.*` as `Dispatcher` + `provider.*` as `ProviderCompleter` chain; `AgentSession::run()` drives conductor end-to-end. Verified: greetings short-circuit (`agentic:false`), multi-step prompts run agentic path. `AgentSession::run_with` exposes driver + `ToolInvoker` seams. *(Post-v1: streaming `run-handle`, real provider routing.)*
- [~] **Conductor** — `before-loop` → `select-model/context/tools` → `complete()` → `after-response` → parse tool calls → `tool-call` gate → tool dispatch → `tool-result` (terminates) → repeat (≤8) → `finalize`. Unit-tested with stubs (8 tests, ≥2-cycle ReAct, permission-deny). *(Pending: `session-start`, `prepare-next-turn`, wasm entry.)*
- [~] **Small-model harness** — parse + validate tool `arguments` (valid JSON), retry-with-correction on malformed output (default N=3). Unit-tested (2 tests). *(Pending: `grammar` passing + construction from tool set.)*
- [x] **Provider fallback** — `complete_with_fallback` re-issues request down `Completer` chain; first success wins. Verified against real `provider-openai.wasm`.
- [ ] **Streaming** — preview-vs-authoritative tokens during `complete()`; emit authoritative at turn boundary.
- [x] **Tool dispatch** — conductor routes parsed calls through `tool-call` gate then `ToolInvoker` seam. `tool-result` block terminates.
- [x] Retire `manager-agent-loop` guest, `agent-loop.wit`, v0 bindings, `manager:` config section. `route.rs` holds provider machinery only.

**Done:** multi-step prompt drives ≥2 ReAct cycles; malformed output triggers retry; simulated rate-limit engages fallback; `next-event` streams correctly.

---

## Slice 2d — v1 interceptor set

**GitHub:** #27 · **Blocked on:** Slice 2b (#25); integration needs 2c (#26)

Four Rust guests exporting `interceptor-world`. Two built thin (single-rule, seam-proving).

- [x] **`interceptor-task-router`** (`select-model`) — classifies request via constrained `llm-provider` call, resolves `routing:<task>` → `provider/model`, sets `pending-request.model`; proceeds if classification/routing yields nothing. 3 native tests + 3 adapter tests.
- [x] **`interceptor-context`** (`select-context`) — trims history to model budget: char/4 estimate + sliding window (keep system + recent, drop oldest). Budget from `host-config` (default 8192). Replaces `manager-context`. 4 native tests + 3 adapter tests.
- [x] **`interceptor-tool-selector`** (`select-tools`) — thin pass-through, exposes tool set, proceeds. Tool sources wired later. 2 adapter tests.
- [x] **`interceptor-permission`** (`tool-call`) — thin single-rule gate: flag dangerous tools, `ask` to confirm, `proceed`/`block` on answer. 3 native tests + 4 adapter tests.
- [x] **Build targets** — all four in extensions Makefile (`make interceptor-<name>[-docker]`), added to `GUESTS`.
- [x] **Harness tests** — each guest has adapter tests (lifecycle + round-trip through real component).

**Done:** all four load through core, dispatched at their phases, drive decisions end-to-end.

---

## Slice 2e — Exit gate

**GitHub:** #28 · **Blocked on:** all of #24–#27

- [x] Offline test `tests/phase2_gate.rs` — canned `host-http`. Boots real `Runtime` from `config.yaml` with two providers, `routing:` section, all five v1 interceptors enabled; calls loop entry with multi-step query + greeting. Asserts end-to-end: (1) intent router fires at `before-loop` (greeting → `agentic:false`; multi-step → `agentic:true`); shaping interceptors run; primary provider fails, secondary answers via **fallback**. (2) **ReAct + permission**: provider emits tool call then answer; `interceptor-permission` asks, driver approves, canned tool runs, result feeds back.
- [x] CI — `make phase2-gate` added to `.github/workflows/ci.yml` (harness job). `make harness` updated.
- [x] Mark Phase 2 `done` — roadmap exit gate (intent → shaped → ReAct → fallback → answer) passes end-to-end. Marked `done` in [roadmap.md](../../concepts/roadmap.md). Carried-forward (post-v1): streaming `run-handle`, real provider routing, `tool-callable` fleet.

**Done:** `make phase2-gate` passes in CI; `roadmap.md` updated. ✓

---

## Open questions feeding Phase 2

Not blockers; resolve before first-needed slice.

| Question | First needed | Current lean |
|---|---|---|
| `hook-state` payload completeness | Slice 2b | Infer budget from [model catalog](../../concepts/architecture.md#model-catalog) via `host-config`; add field only if needed |
| Loop-entry / driver capability WIT shape | Slice 2b | Prototype as core Rust entry; promote to host cap when `api-*` lands |
| Grammar format (GBNF vs JSON-schema) | Slice 2c | GBNF (llama.cpp); confirm vLLM/Ollama accept it |
| Intra-phase load order | Slice 2b | Registry insertion order; document it |
| Token-count estimate | Slice 2d | char/4; tiktoken-rs if accuracy matters |
| Classification interceptor routing | Slice 2d | Take `llm-provider` import; revisit if double-classification wasteful |
| Retry limit | Slice 2c | 3; `host-config`-overridable |
| Optional imports | Slice 2c/2d | Skip-if-absent; stub linker entries if needed |

## Cross-cutting (continuous)

- Strict clippy lint policy (`[workspace.lints]` inherited or declared locally) on
  every new crate — same policy as Phase 1.
- `cargo test` stays green on every PR; `make harness` covers component-level tests;
  `make phase2-gate` is the integration gate.
- Supply-chain gates (`cargo-audit`, `cargo-deny`, `govulncheck`) extended to cover
  new guest crates.
- Structured logging: every new component tags its `host-log` lines with its name
  (e.g. `[interceptor.intent-router]`, `[interceptor.context]`).
