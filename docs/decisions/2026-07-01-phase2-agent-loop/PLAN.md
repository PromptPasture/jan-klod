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
| Slice 2a — Intent router logic (recast into `interceptor-intent-router`) | `in-progress` |
| Slice 2b — `interceptor` contract + core-native dispatch framework | `not-started` |
| Slice 2c — Core loop mechanism (conductor, harness, fallback, run entry) | `not-started` |
| Slice 2d — v1 interceptor set (task-router, context, tool-selector, permission) | `not-started` |
| Slice 2e — Exit gate | `not-started` |

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
//          | finalize | prepare-next-turn
```

**Dispatch is core-native and ordering is structural:** the core calls each enabled
interceptor's `intercept` at the phases it lists in `subscribed-phases()`, across
phases in enum order and within a phase in deterministic load order. `jan-klod.yaml`
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

Recast (pending):
- [ ] **New guest `src/extensions/interceptor-intent-router/`** exporting
  `interceptor-world`; move the `router` module in verbatim.
- [ ] **`subscribed-phases()` → `[before-loop]`**; implement `intercept` for the
  `before-loop(user-turn)` case: `simple` → `block` (short-circuit the agentic loop,
  carrying the direct answer), `agentic` → `proceed`.
- [ ] **LLM classifier via the routed `llm-provider` import** (now available on
  `interceptor-world`), replacing the in-manager provider call.
- [ ] **Build** — `make interceptor-intent-router[-docker]`.

**Definition of done:** the guest loads through the core, is dispatched only at
`before-loop`, classifies greetings/acks as `simple` (no model call) and multi-step
prompts as `agentic`; the existing 17 router unit tests pass unchanged in the new
crate.

---

## Slice 2b — `interceptor` contract + core-native dispatch framework

**GitHub:** #25 · **Blocked on:** —

Finalize the contract and build the host machinery that drives it. This is the
foundation both the core loop (2c) and every interceptor (2a, 2d) hang off.

- [ ] **Finalize [`wit/interceptor.wit`](../../../wit/interceptor.wit)** (drafted;
  validates via `wasm-tools component wit wit/`) — confirm the `hook-state` payload
  records against the loop's needs; add the `run-handle`/driver **loop-entry**
  interface (`run` / `next-event` / `cancel` / `provide-answer` for `ask` resume /
  steering + follow-up injection) as a **core-exposed** surface *(planned WIT;
  exercised in Phase 2 through a core Rust entry, not yet a host capability)*.
- [ ] **`bindgen!` the interceptor world** in core; generate the host-side caller.
- [ ] **Dispatch engine** (`jan_klod_core::intercept`): given a phase and a mutable
  loop state, call each enabled+subscribed interceptor in order, apply its
  `decision` — `proceed` (no-op), `replace` (swap the phase state), `block`
  (short-circuit with reason), `ask` (suspend → surface prompt on the run-handle →
  resume on `provide-answer`, re-invoking the same interceptor with `answer` set).
- [ ] **Error/trap policy** — wrap each `intercept` call: on `Err`/trap, **fail
  closed at `tool-call`** (treat as `block`), fail-open-with-log elsewhere; emit the
  offending interceptor id on the `host-event` bus.
- [ ] **Registration** — read the interceptor enable/disable set from `jan-klod.yaml`
  (on/off only); resolve `subscribed-phases()` at boot; establish deterministic
  load order for intra-phase sequencing.
- [ ] **`configuration.md` note** — document the interceptor enable/disable keys
  (the one remaining open item from the decision record).
- [ ] **Retire** `context-manager.wit` and mark `agent-manager.wit` / `agent-loop.wit`
  superseded (remove from the package once 2c/2d no longer reference them).

**Definition of done:** an offline test registers two stub interceptors on the same
phase and one on another, and asserts: order (across + within phase), each decision
variant is applied, a `tool-call` trap fails closed, and an `ask` round-trips through
a canned driver. `wasm-tools component wit wit/` stays green.

---

## Slice 2c — Core loop mechanism

**GitHub:** #26 · **Blocked on:** Slice 2b (#25)

Build the thin conductor in core. Zero policy — all decisions are delegated to the
dispatch engine from 2b.

- [ ] **Loop entry** — core Rust `run_agent(session, user-message)` returning a
  `run-handle`; `next-event` streams `text-delta` / `tool-invoked` / `tool-result` /
  `warning` / `done`; `cancel` / `close`; a `pending-prompt` event + `provide-answer`
  for the `ask` flow; steering + follow-up queue.
- [ ] **Conductor** — `session-start` (once) → `before-loop` → per turn:
  `select-model` → `select-context` → `select-tools` (dispatch phases, assembling
  `pending-request`) → `complete()` → `after-response` → parse → `tool-call` →
  tool dispatch → `tool-result` (honour `terminate`) → `prepare-next-turn` → loop;
  on exit `finalize`.
- [ ] **Small-model harness (core mechanism)** — pass the `grammar` on
  `completion-request` (provider executes it); grammar **construction** default
  (derive from the active tool set after `select-tools`), overridable by an
  interceptor; **parse + structural validation**; **retry-with-correction** on
  malformed output (configurable N via `host-config`, default 3; no silent spiral).
- [ ] **Provider fallback (core mechanism)** — on `provider-error`
  (`rate-limited` / `transient` / `model-not-found`) re-issue the *same* request
  down the `providers:` list (each model in a provider → next provider → exhausted →
  `AgentError`); emit a `warning`; per-request (primary recovers next request).
- [ ] **Streaming** — preview-vs-authoritative: stream tokens live during each
  `complete()`; emit the authoritative message at the turn boundary (which
  `after-response`/`finalize` may have `replace`d).
- [ ] **Tool dispatch** — route `tool-call` requests to the routed `tool-callable`
  extensions (skip-if-absent in v1); honour a tool-result `terminate`.
- [ ] **Retire the `manager-agent-loop` guest** and the `agent-loop-world` stub path
  in core routing.

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

- [ ] **`interceptor-task-router`** (`select-model`) — classify the request into a
  task type (built-in list + user-defined from `host-config`) via a constrained
  `llm-provider` call; resolve the `routing:` entry (`<provider-instance>/<model>`)
  and set `pending-request.model`. Subsumes the old task-routing domain logic.
- [ ] **`interceptor-context`** (`select-context`) — the reworked context manager,
  history + compression handled **internally**: keep session history (in-memory,
  persistence deferred to Phase 3), trim to the chosen model's budget
  (char/4 estimate v1; drop oldest turns; summarisation via `llm-provider`
  deferred behind the same interface). Replaces `manager-context` +
  `context-manager.wit`.
- [ ] **`interceptor-tool-selector`** (`select-tools`) — **thin**: a working
  pass-through that exposes the full active tool set (from `mcp-registry` /
  `tool-callable` when present), leaving per-step narrowing as a later refinement.
- [ ] **`interceptor-permission`** (`tool-call`) — **thin**: a single-rule gate
  (e.g. confirm on a configured dangerous-command pattern) that returns `ask` to the
  driver and `block`/`proceed` on the answer — exercising the `ask` round-trip and
  the fail-closed policy.
- [ ] **Build targets** — `make interceptor-<name>[-docker]` for each.
- [ ] **Harness tests** — each guest: lifecycle + `subscribed-phases` + one
  `intercept` round-trip; skip when the component is not staged.

**Definition of done:** all four load through the core, are dispatched only at their
phases, and drive their decisions end-to-end in the harness (task-router sets a model;
context trims a synthetic long history; tool-selector passes tools through; permission
asks then blocks/allows).

---

## Slice 2e — Exit gate

**GitHub:** #28 · **Blocked on:** all of #24–#27

- [ ] Offline test `tests/phase2_gate.rs` — canned `host-http`, no network / API key:
  - Boot the real `Runtime` from a `jan-klod.yaml` with two provider instances, a
    `routing:` section, and the v1 interceptors enabled.
  - Call the core **loop entry** (`run_agent`).
  - Submit a multi-step user query.
  - Assert: intent router fires at `before-loop`; the request-shaping phases assemble
    the request (model set, history trimmed, tools selected); ≥1 ReAct cycle
    completes; retry+correction fires on a simulated malformed output; provider
    fallback engages on a simulated failure; the permission `ask` round-trips; the
    loop returns a grounded answer via `next-event` → `done`.
- [ ] CI — `make phase2-gate` added to `.github/workflows/ci.yml`; must pass without
  network access.
- [ ] **Mark Phase 2 `done`** here and in [roadmap.md](../../concepts/roadmap.md);
  begin Phase 3 planning.

**Definition of done:** `make phase2-gate` passes in CI; `roadmap.md` status tracker
updated to `done`.

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
