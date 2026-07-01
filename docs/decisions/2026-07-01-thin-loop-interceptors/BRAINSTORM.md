---
type: decision
title: Thin Loop + Interceptor Middleware
description: Move the agent-loop mechanism into core (Rust) and express every agent decision as a sandboxed interceptor extension, adapting Pi's thin-loop design to jan-klod's sandbox + small-model foundation.
method: comparative analysis
date: "2026-07-01"
tags: [agent-loop, interceptors, architecture, small-model, wasm, core]
created: 2026-07-01
updated: 2026-07-01
related:
  - ../../concepts/architecture.md
  - ../../concepts/small-model-harness.md
  - ../../concepts/contracts.md
  - ../2026-07-01-phase2-agent-loop/PLAN.md
---

# Brainstorm — Thin Loop + Interceptor Middleware

## Goal

Keep jan-klod's foundation — **sandbox + small models + everything is a WASM
component + nothing is trusted** — but adopt Pi's *astonishingly thin* agent loop
(`stream assistant → if tool calls, run them → loop`) by moving the loop
**mechanism** into core and expressing every agent **decision** (intent routing,
task classification, tool selection, context compression, provider fallback,
permission policy) as a **sandboxed interceptor extension**.

## Context

Reviewed [earendil-works/pi](https://github.com/earendil-works/pi) against the
jan-klod docs. Pi and jan-klod make opposite bets:

- **Pi** — frontier models + trust. No sandbox (containerize externally). The
  agent loop in `pi-agent-core` is a thin conductor; *all* extensibility lives in
  an ordered, return-carrying **lifecycle-hook / middleware** chain
  (`before_agent_start`, `context`, `tool_call` (block), `tool_result` (modify),
  `prepare_next_turn`, steering / follow-up messages). Permission gates, path
  protection, custom compaction — all are just extensions hooking in.
- **jan-klod** — sandbox + small models. Everything is a WASM component behind
  WIT; nothing is trusted. The current Phase 2 design makes `manager-agent-loop`
  a monolithic extension carrying intent router + step controller + decomposer +
  fallback + routing all at once.

Pi's real innovation is *where it puts extensibility*, not how it runs the loop.
The current `host-event` bus is **fire-and-forget pub/sub** (publish + poll) — it
lets an extension *observe* the loop but not *shape* it (block a tool, rewrite the
request, return a classification). That gap is the crux this brainstorm resolves.

## Agenda

1. How does a "concern" extension plug into a thin loop? (hook mechanism)
2. Where does the loop mechanism live, given "zero agent behaviour in core"?
3. Where do the irreducible small-model harness pieces sit?
4. What do we build in v1?

## Ideas Considered

### Hook mechanism — how concerns attach to the loop

- **Typed pipeline stages** — the loop imports optional WIT interfaces and calls
  them at fixed points (`classify()` → `select-tools()` → `compress()` →
  `complete()`).
  - **Benefits:** maximally typed, strongly sandboxed, very WIT-native.
  - **Trade-offs:** adding a *new kind* of concern means changing the loop's WIT
    world — the loop must know every stage type up front.
- **Interceptor middleware** *(chosen)* — a new synchronous, ordered,
  return-carrying host primitive (block / modify / passthrough), alongside (not
  replacing) the pub/sub bus. The loop emits lifecycle points; any extension
  subscribes and returns a decision.
  - **Benefits:** maximally extensible — new concerns need no loop change; direct
    map of Pi's proven model.
  - **Trade-offs:** less typed; new host machinery; "spooky action" is harder to
    reason about than an explicit call.
- **Hybrid** — typed stages for known decision points + an interceptor chain for
  cross-cutting policy.
  - **Trade-offs:** two mechanisms to build and document.

### Where the loop mechanism lives

- **Loop as an extension** (status quo) — `manager-agent-loop` stays a swappable,
  polyglot WASM component.
  - **Benefits:** loop is untrusted and language-agnostic like everything else.
  - **Trade-offs:** with the interceptor model, the loop has *no behaviour left* —
    it becomes pure mechanism, so sandboxing/swapping it buys little, and a WASM
    loop driving a WASM interceptor chain means marshalling across the boundary
    twice.
- **Loop mechanism in core** *(chosen)* — core owns the thin conductor natively in
  Rust and drives the interceptor chain.
  - **Benefits:** "zero agent behaviour in core" is *preserved in spirit* — the
    loop holds no policy; every decision is a sandboxed interceptor. Simpler and
    faster (native dispatch, no double boundary crossing). Honest Pi mapping (Pi's
    loop is runtime, not an extension).
  - **Trade-offs:** the loop conductor is no longer swappable/polyglot. Accepted —
    it is ~200 lines of rarely-changing mechanism, and interceptors + `terminate`
    + steering give the topology flexibility a swappable loop would have.

### Where the small-model harness sits

The harness has two irreducible pieces entangled with the loop's innards:
constrained-decoding **grammar** and **retry/validate-on-malformed-output**.

- **Core mechanism, with seams** *(chosen — option #1)* — parse + structural
  validation + retry-with-correction are fixed core mechanism (retry is iteration
  control = mechanism). The loop always passes a `grammar` to `complete()` (the
  provider executes it, as today). Grammar construction and retry policy (on/off,
  N, hint template) are tunable via `host-config` and overridable through the
  general `prepare-request` interception point that every request-shaping
  interceptor already uses — no harness-specific seam.
  - **Benefits:** small-model reliability is always on and can't be silently
    disabled by a missing interceptor; the grammar is just one more field on the
    request the existing seam can touch.
- **Pure Pi-thin core; harness = interceptors** (option #2) — core does only parse
  + dispatch; grammar and retry/validate are interceptors added for small models,
  removed for frontier.
  - **Trade-offs:** retry-as-interceptor must drive loop iteration from outside
    (awkward); a missing interceptor silently degrades reliability.
- **All harness in core, no seams** (option #3) — core builds the whole request
  with fixed logic; only flat config tunes it.
  - **Trade-offs:** *contradicts the interceptor decision* — tool-selection and
    compression **are** request-shaping and must run at the "before the provider
    call" point. Removing that seam forces them back into core, collapsing the
    design into a monolith. Effectively off the table.

## Outcomes

### Summary

Adopt Pi's thin loop, but as **core mechanism** rather than an extension. A new
**interceptor-middleware** host primitive (synchronous, ordered, return-carrying —
one generic `intercept` over an ordered `phase` enum, returning
`proceed | replace | block | ask`) sits alongside the existing observation-only
`host-event` bus. Every agent *decision* becomes a **sandboxed interceptor
extension**. The small-model harness's irreducible pieces stay as fixed core
mechanism, tuned through `host-config` and the request-shaping phases
(`select-model`/`-context`/`-tools`). The
sandbox / nothing-trusted posture is unchanged: core is the trusted host by
definition; all providers, tools, stores, and interceptors remain sandboxed WASM.

### Decisions

1. **Foundation unchanged.** Sandbox + small models + everything-is-a-WASM-component
   + nothing-trusted all stand.
2. **Thin loop mechanism moves into core (Rust).** Retires `manager-agent-loop` as
   an extension. The loop is a conductor: `stream → tools → loop`, plus interceptor
   dispatch, streaming handles, cancel, and the steering / follow-up queue. It
   holds **zero agent behaviour**. Not swappable/polyglot — accepted trade.
3. **Extension mechanism = interceptor middleware.** A new synchronous, ordered,
   return-carrying host primitive driven natively by core, *alongside* the
   fire-and-forget `host-event` bus (which stays, for observation). Interceptors
   are sandboxed WASM components.
4. **Lifecycle interception points = an ordered `phase` enum.** One generic
   `intercept` function over a `phase`, not a function per stage — adding a point is
   a new enum case, so the design favours **many narrow phases over few broad ones**
   (each phase is a real state transition ⇒ ordering between concerns is *structural*,
   not a config-fragile contract inside one big phase). v1 phases, in order:
   `session-start` (once/session), `before-loop` (intent — may short-circuit),
   `select-model` · `select-context` · `select-tools` (the old `prepare-request`,
   split into ordered request-shaping sub-phases), `after-response` (raw output repair,
   pre-parse), `tool-call`, `tool-result`, `finalize` (shape the authoritative answer),
   `prepare-next-turn`; plus steering + follow-up injection. The return `decision` is
   `proceed | replace | block | ask`. **`ask`** routes a question through the loop to
   the attached driver and resumes on the answer — this is how a permission gate
   confirms with the user without touching a UI (resolves the old UI-gap question).
   `intercept` returns `result<decision, interceptor-error>`; on error/trap the host
   **fails closed at `tool-call`**, fails-open-with-log elsewhere. Ordering is *not*
   configurable — `jan-klod.yaml` only enables/disables; phases order across, load
   order within. Drafted as [`wit/interceptor.wit`](../../../wit/interceptor.wit).
5. **Small-model harness split (option #1).** Parse + structural validation +
   retry-with-correction = fixed core mechanism. Loop always passes a `grammar` to
   `complete()` (provider executes it). Grammar construction and retry policy are
   tunable via `host-config` and the request-shaping phases — no
   harness-specific seam.
6. **Also adopt from Pi:** tool-result `terminate` flag (a tool can end the loop);
   steering + follow-up messages (mid-run and post-stop injection); the
   `AgentMessage`-superset idea (custom message types persisted in the transcript,
   filtered/converted at the LLM boundary).
7. **v1 scope = full set**, all as interceptors: intent router, task classifier /
   routing, tool selector (built thin — a working pass-through), context compressor
   (the reworked `manager-context`), and a sample permission gate (built thin — a
   single-rule gate). Thin-but-real for the two without a concrete driver yet (tool
   selector, permission gate) to prove the seam without speculative machinery.
   *(Provider fallback was initially grouped here but reclassified to core mechanism
   in the post-review reconciliation below.)*

### Post-review reconciliation (2026-07-01)

A `/review` pass surfaced three points asserted as settled that were either
contradictory or genuinely open. Resolved:

1. **Dispatch is core-native (no `host-hook` import).** The core loop invokes each
   interceptor's *exported* `interceptor` interface directly at each phase;
   interceptors are **enabled/disabled via `jan-klod.yaml`** (ordering is structural,
   not configured — see Decision 4). There is no `host-hook.wit` capability an
   extension imports — an earlier draft that added one contradicted the "core drives
   natively" rationale and is dropped.
2. **Generic hook only for `interceptor-context`.** History trimming / compression
   is *internal* to `interceptor-context` behind the generic `interceptor` hook; the
   loop does not call `context-manager` directly. `context-manager.wit` is subsumed
   (kept, if at all, only as an internal type source), like `agent-manager.wit`.
3. **Provider fallback is core mechanism, not an interceptor.** It re-issues the
   *same* failed request on the next provider (an on-provider-error retry, the same
   category as retry/validate) — which a `prepare-next-turn` interceptor cannot do.
   `interceptor-task-router` therefore does classification + routing only.

### Open Questions

- ~~**Interceptor WIT shape**~~ **Resolved.** Drafted as
  [`wit/interceptor.wit`](../../../wit/interceptor.wit): one generic `intercept` over a
  `phase` enum + `subscribed-phases`, return `result<decision, interceptor-error>`
  (`proceed | replace | block | ask`), phase-keyed `hook-state` variant. Streaming
  composes via the **preview-vs-authoritative** rule — streamed tokens are a preview;
  the loop emits the authoritative message at the boundary, which `after-response`/
  `finalize` may `replace`, and drivers reconcile. Interception is at turn boundaries.
- ~~**UI gap.**~~ **Resolved** by the `ask` decision: an interceptor returns
  `ask(user-prompt)`, the loop suspends and routes the question to the attached driver
  (which prompts in its own idiom — TUI/chat/`api-*`), then re-invokes the interceptor
  with the answer. `default-answer` covers the headless/no-driver case. The permission
  gate never touches a UI.
- ~~**Interceptor ordering / registration.**~~ **Resolved:** ordering is structural,
  not configured — across phases by the `phase` enum, within a phase by deterministic
  load order. `jan-klod.yaml` only enables/disables (still needs a `configuration.md`
  note documenting the on/off keys).
- **`interceptor-task-router` scope** — one extension doing classification + routing,
  or split. Deferred to implementation.
- **Carry-overs** from the Phase 2 plan: `whatlang` for language detection, grammar
  format (GBNF vs JSON-schema), and the token-count estimate strategy for the
  context compressor.

## Next Steps

Re-architect the [Phase 2 plan](../2026-07-01-phase2-agent-loop/PLAN.md) around
this decision (thin core loop + interceptor framework + the full interceptor set)
and produce an implementation plan. The already-built Slice 2a intent router is
re-cast as the first interceptor.
