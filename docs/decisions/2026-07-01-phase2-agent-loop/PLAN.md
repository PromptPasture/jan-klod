# Phase 2 Plan — First Real Value: the Agent Loop

Living execution checklist for Phase 2 of the
[Roadmap](../../concepts/roadmap.md). Update the flags here and in the roadmap
[Status tracker](../../concepts/roadmap.md#status-tracker) as work proceeds.

Prerequisite: Phase 1 exit gate passed (2026-07-01).
Relevant background: [small-model-harness.md](../../concepts/small-model-harness.md) ·
[architecture.md](../../concepts/architecture.md) ·
[contracts.md](../../concepts/contracts.md).

## Goal

The runtime does something useful end-to-end: a user query enters
`manager-agent-loop`, the intent router classifies it, the step controller drives
a ReAct cycle, `manager-context` keeps history within budget, provider fallback
and task routing resolve the right model, and the loop returns a grounded answer.

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Item | Flag |
|---|---|
| Slice 2a — Intent router | `not-started` |
| Slice 2b — ReAct step controller + retry/validate | `not-started` |
| Slice 2c — `manager-context` extension | `not-started` |
| Slice 2d — Provider fallback + task routing | `not-started` |
| Slice 2e — Exit gate | `not-started` |

## Architecture context

`manager-agent-loop` currently implements the minimal `agent-loop-world` (v0
one-shot stub from Phase 1). Phase 2 upgrades it to the full `agent-manager-world`:

```wit
world agent-manager-world {
    import host-log;
    import host-config;
    import host-event;
    import host-storage;
    import llm-provider;
    import context-manager;     -- satisfied by manager-context (Slice 2c)
    import skill-registry;      -- optional, skip-if-absent in v1
    import mcp-registry;        -- optional, skip-if-absent in v1
    import tool-callable;       -- optional, skip-if-absent in v1
    import agent-delegate;      -- optional, skip-if-absent in v1

    export extension-lifecycle;
    export agent-manager;
}
```

The `agent-manager` interface uses a **poll-based streaming handle** (same
pattern as `llm-provider`):

```wit
run:        func(session, user-message, task-type-hint) -> result<run-handle, agent-error>
next-event: func(handle) -> option<agent-event>
cancel:     func(handle)
close:      func(handle)
```

`agent-event` variants: `text-delta`, `tool-invoked`, `tool-result`,
`subtask-dispatched`, `subtask-done`, `done`, `warning`.

The `manager-context` extension implements:

```wit
append:      func(session, msg)  -> result<_, context-error>
messages:    func(session, token-budget) -> result<list<message>, context-error>
clear:       func(session)       -> result<_, context-error>
token-count: func(session)       -> result<u32, context-error>
```

Core routing (`jan_klod_core::route`) must be extended to wire
`manager-agent-loop`'s `context-manager` import into the `manager-context`
instance, alongside the existing `llm-provider` / `memory-store` routing.

---

## Slice 2a — Intent router

**GitHub:** #24

Implement the layered intent router inside `manager-agent-loop` as the first gate
before entering the ReAct step controller.

- [ ] **Language detection** — pure-Rust crate (no model call); non-English
  bypasses the heuristic tier and goes directly to the LLM classifier.
  Candidate: `whatlang`.
- [ ] **Heuristic tier** (English, microseconds) — ~50 rules grouped by category
  (greetings, farewells, affirmations, meta-queries, clarifications, short
  inputs). Emits `simple` or `pass`. Rules are organised in named groups, not a
  flat list — one new rule = one line in the right group.
- [ ] **LLM classifier tier** — single constrained-decoding call to the active
  `llm-provider` (`grammar` field forces one token: `simple` | `agentic`). Runs
  for all languages. Reuses the already-loaded provider; no embedding model.
- [ ] Router result drives `run`: `simple` → direct completion, no loop;
  `agentic` → step controller (Slice 2b).

**Definition of done:** `run` correctly classifies greetings and single-fact
questions as `simple` without entering the agent loop; multi-step prompts reach
the step controller; verified by offline unit tests (canned provider).

---

## Slice 2b — ReAct step controller + retry/validate

**GitHub:** #25 · **Blocked on:** Slice 2a (#24)

Upgrade `manager-agent-loop` from `agent-loop-world` to `agent-manager-world` and
build the real agentic path.

- [ ] **World migration** — change the `wit_bindgen::generate!` world from
  `agent-loop-world` to `agent-manager-world`; implement the `agent-manager`
  export interface (`run`/`next-event`/`cancel`/`close`) using the poll-handle
  pattern; retire the `agent-loop` stub.
- [ ] **Step controller** — per-step loop:
  - Calls `context-manager.messages(session, token_budget)` to obtain a
    budget-trimmed history (Slice 2c must be routed in first; use a stub
    passthrough until 2c lands).
  - Selects tools relevant to the current step (dynamic injection — not all tools
    at once).
  - Builds a per-step prompt with few-shot examples scoped to the chosen tools.
  - Issues a constrained-decoding `llm-provider.complete` call (`grammar` field
    forces valid JSON action output).
- [ ] **ReAct cycle** — parse the action; if it is `final-answer`, extract the
  text and emit `done`; if it is a tool call, dispatch the tool and emit
  `tool-invoked` / `tool-result`, then loop back.
- [ ] **Retry/validate** — on malformed action output: inject a correction hint
  into the next prompt and retry (configurable N, default 3). After N failures
  emit `AgentError::InvalidOutput`. No silent retry spiral.
- [ ] **Answer extractor** — on `final-answer`, emit the text as `text-delta`
  chunks then `done`; call `context-manager.append` with the assistant turn;
  close the stream handle.

**Definition of done:** a multi-step prompt drives at least two ReAct cycles
through a canned `host-http`; malformed output triggers retry + correction;
`next-event` streams events correctly; verified by `make harness`.

---

## Slice 2c — `manager-context` extension

**GitHub:** #26 · **Blocked on:** Slice 2b (#25) for routing wiring

New Rust `wit-bindgen` guest implementing the `context-manager` WIT interface.

- [ ] **Extension scaffold** — `src/extensions/manager-context/` following the
  same crate layout as `store-memory` and `provider-openai`; strict clippy lint
  policy declared locally.
- [ ] **Session store** — `HashMap<session-id, Vec<message>>` backing all
  sessions (in-memory for now; persistence deferred to Phase 3).
- [ ] **`append`** — push a `message` onto the session history.
- [ ] **`messages`** — return the session history trimmed to `token-budget`.
  Strategy: token-count estimate (character / 4, implementation-defined) →
  drop oldest turns until within budget. Compression (summarisation) is deferred;
  truncation is the v1 strategy.
- [ ] **`clear`** — discard all history for a session.
- [ ] **`token-count`** — return the estimate for the current history.
- [ ] **Core routing** — extend `jan_klod_core::route` to wire
  `manager-agent-loop`'s `context-manager` import into the `manager-context`
  instance alongside the existing `llm-provider` / `memory-store` routing.
- [ ] **Build** — `make manager-context` and `make manager-context-docker`
  targets (`cargo build --target wasm32-wasip2`).
- [ ] **Harness test** — lifecycle round-trip + `append`/`messages`/`clear`
  round-trip in `component_harness.rs`; skip when component not staged.

**Definition of done:** component loads through the core, passes lifecycle, and
the step controller in `manager-agent-loop` calls it to trim a synthetic long
history; offline test verifies the round-trip.

---

## Slice 2d — Provider fallback + task routing

**GitHub:** #27 · **Blocked on:** Slice 2b (#25)

Wire the two routing features from the architecture spec into the core and the
agent-loop guest. These are domain-logic concerns of `manager-agent-loop`;
`jan-klod.yaml` sections `providers` and `routing` are passed verbatim to the
manager via `host-config` — the core does not validate them.

**Provider fallback:**

- [ ] `jan-klod.yaml` `providers:` list (provider instance → ordered model list)
  read from `host-config` inside `manager-agent-loop`.
- [ ] Fallback logic in the guest: try each model in the current provider →
  move to the next provider → if all exhausted return `AgentError::ProviderFailed`
  (no silent spiral). Triggers: `rate-limited`, `transient`, `model-not-found`
  from `provider-error`.
- [ ] Fallback is per-request: if the primary recovers, the next request uses it.
- [ ] Emit a `warning` `agent-event` when fallback engages.

**Task routing:**

- [ ] `jan-klod.yaml` `routing:` section maps task-type labels to
  `<provider-instance>/<model>`; read from `host-config`.
- [ ] Intent router (Slice 2a) or step controller sets the task-type; the manager
  resolves the routing entry and selects the correct provider instance for that
  request. Empty `task-type-hint` → auto-classify.
- [ ] Built-in task types: `code-generation`, `code-review`, `file-edit`,
  `web-search`, `research`, `reasoning`, `planning`, `chat`, `clarification`,
  `agent-delegation`. User-defined types accepted without code changes.

**Definition of done:** with two providers in `jan-klod.yaml`, a simulated
`rate-limited` on the first causes transparent retry on the second; a `reasoning`
request routes to the configured Anthropic model; both verified by offline tests.

---

## Slice 2e — Exit gate

**GitHub:** #28 · **Blocked on:** all of #24–#27

Verify the Phase 2 exit condition end-to-end.

- [ ] Offline test `tests/phase2_gate.rs` — canned `host-http`, no network / API
  key required:
  - Boot the real `Runtime` from a `jan-klod.yaml` with two provider instances
    and a `routing:` section.
  - Call `route_agent_manager` (analogous to Phase 1 `route_agent_loop`).
  - Submit a multi-step user query.
  - Assert the intent router fires, at least one ReAct cycle completes, context
    compression is called, provider fallback engages on a simulated failure, and
    the loop returns a grounded answer via `next-event` → `done`.
- [ ] CI — `make phase2-gate` added to `.github/workflows/ci.yml`; must pass
  without network access.
- [ ] **Mark Phase 2 `done`** in `PLAN.md` (this file) and
  [roadmap.md](../../concepts/roadmap.md); begin Phase 3 planning.

**Definition of done:** `make phase2-gate` passes in CI; `roadmap.md` status
tracker updated to `done`.

---

## Open questions feeding Phase 2

These are not blockers for Phase 2 but should be resolved before the slices that
first need them.

| Question | First needed | Current lean |
|---|---|---|
| `whatlang` vs alternative for language detection | Slice 2a | `whatlang` (pure-Rust, no model) — confirm crate quality before committing |
| Constrained-decoding grammar format (GBNF vs JSON-schema) | Slice 2b | GBNF (llama.cpp compatible); confirm vLLM / Ollama accept the same `grammar` field |
| Token-count estimate strategy for `manager-context` v1 | Slice 2c | char / 4 heuristic; tiktoken-rs if accuracy matters — decide at implementation |
| Optional imports (`skill-registry`, `mcp-registry`, `tool-callable`, `agent-delegate`) | Slice 2b | Skip-if-absent in v1; stub satisfying the WIT import with no-ops if Wasmtime requires a concrete linker entry |
| Retry limit N | Slice 2b | 3 (from architecture open questions); bake into `host-config` section so users can override |
| ACP delegation timeout | Phase 4 | ~30 s (carry-over); not needed in Phase 2 |

## Cross-cutting (continuous)

- Strict clippy lint policy (`[workspace.lints]` inherited or declared locally)
  on every new crate — same policy as Phase 1.
- `cargo test` must stay green on every PR; `make harness` covers component-level
  tests; `make phase2-gate` is the integration gate.
- Supply-chain gates (`cargo-audit`, `cargo-deny`, `govulncheck`) already wired
  in CI — extend to cover new guest crates.
- Structured logging: every new component tags its `host-log` lines with its
  component name (e.g. `[manager.context]`, `[manager.agent-loop]`).
