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

Keep jan-klod's foundation — **sandbox + small models + everything WASM + nothing trusted** — 
but adopt Pi's thin loop (`stream → if tools, run → loop`) by moving loop **mechanism** 
into core and expressing every agent **decision** (intent routing, task classification, 
tool selection, context compression, fallback, permission) as **sandboxed interceptor extension**.

## Context

Reviewed [earendil-works/pi](https://github.com/earendil-works/pi) vs. jan-klod. Opposite bets:

- **Pi** — frontier models + trust. No sandbox. Agent loop is thin conductor; *all* 
  extensibility via ordered, return-carrying **lifecycle-hook chain** 
  (`before_agent_start`, `context`, `tool_call`, `tool_result`, `prepare_next_turn`, steering). 
  Permission, path protection, compaction all extend.
- **jan-klod** — sandbox + small models. Everything WASM/WIT, nothing trusted. Current 
  `manager-agent-loop` is monolithic (router + controller + decomposer + fallback + routing).

Pi's innovation: *where* extensibility lives. Current `host-event` bus is **fire-and-forget pub/sub** — 
extensions *observe* but don't *shape* (can't block tools, rewrite requests, classify). This gap 
resolves here.

## Agenda

1. How does a "concern" extension plug into a thin loop? (hook mechanism)
2. Where does the loop mechanism live, given "zero agent behaviour in core"?
3. Where do the irreducible small-model harness pieces sit?
4. What do we build in v1?

## Ideas Considered

### Hook mechanism

- **Typed pipeline stages** — loop imports optional WIT interfaces at fixed points.
  - Benefits: maximally typed, strongly sandboxed. Trade-offs: new concerns require loop WIT change.
- **Interceptor middleware** *(chosen)* — synchronous, ordered, return-carrying primitive (alongside pub/sub). Loop emits lifecycles; extensions subscribe + return decision.
  - Benefits: maximally extensible; direct Pi map. Trade-offs: less typed; new host machinery.
- **Hybrid** — typed stages + interceptor chain for cross-cutting policy.
  - Trade-offs: two mechanisms.

### Where the loop mechanism lives

- **Loop as extension** (status quo) — `manager-agent-loop` swappable WASM component.
  - Benefits: untrusted, language-agnostic. Trade-offs: no behaviour left, WASM-to-WASM marshalling twice.
- **Loop mechanism in core** *(chosen)* — core owns thin conductor, drives interceptor chain.
  - Benefits: "zero policy in core" preserved; simpler, faster (native dispatch). Pi-honest (Pi's loop is runtime). Trade-offs: conductor not swappable, but ~200 lines rarely-changing; interceptors + `terminate` + steering give flexibility.

### Where the small-model harness sits

Harness has two irreducible pieces: constrained **grammar** and **retry-on-malformed-output**.

- **Core mechanism with seams** *(chosen)* — parse + validate + retry-with-correction fixed in core. Loop passes `grammar` to `complete()`. Grammar construction and retry policy tunable via `host-config` + `prepare-request` interception.
  - Benefits: small-model reliability always on; can't disable silently.
- **Pure Pi-thin; harness = interceptors** — core does parse + dispatch only; grammar/retry are add-on interceptors.
  - Trade-offs: retry-as-interceptor drives iteration awkwardly; missing interceptor degrades silently.
- **All harness in core, no seams** — core builds request with fixed logic; flat config tunes.
  - Trade-offs: contradicts interceptor decision; tool-selection/compression *must* run at "before provider". Effectively off table.

## Outcomes

### Summary

Adopt Pi's thin loop as **core mechanism**. New **interceptor-middleware** host primitive 
(synchronous, ordered, return-carrying: one generic `intercept` over `phase` enum, returning 
`proceed | replace | block | ask`) sits alongside pub/sub `host-event` bus. Every agent 
*decision* is **sandboxed interceptor extension**. Small-model harness irreducibles stay as 
fixed core mechanism, tuned via `host-config` + request-shaping phases. Sandbox/nothing-trusted 
posture unchanged: core is trusted host by definition; all providers, tools, stores, interceptors remain sandboxed WASM.

### Decisions

1. **Foundation unchanged** — sandbox + small models + everything-WASM + nothing-trusted.
2. **Thin loop mechanism in core (Rust).** Retires `manager-agent-loop` as extension. Conductor: `stream → tools → loop`, plus interceptor dispatch, streaming, cancel, steering/follow-up. **Zero agent behaviour**. Not swappable — accepted trade.
3. **Extension mechanism = interceptor middleware** — synchronous, ordered, return-carrying host primitive (alongside `host-event` pub/sub). Sandboxed WASM components.
4. **Lifecycle = ordered `phase` enum** — one generic `intercept` over `phase` (not per-stage function). Many narrow phases favor structural ordering. v1 phases: `session-start`, `before-loop` (intent, may short-circuit), `select-model/context/tools` (old `prepare-request`, split), `after-response`, `tool-call`, `tool-result`, `finalize`, `prepare-next-turn`. Decision: `proceed | replace | block | ask`. **`ask`** routes question to driver, resumes on answer — permission gates without UI. Returns `result<decision, interceptor-error>`; fails closed at `tool-call`, open elsewhere. Ordering structural (phases + load order), not configured. [`wit/interceptor.wit`](../../../wit/interceptor.wit).
5. **Small-model harness (option #1)** — parse + validate + retry-with-correction = fixed core. Loop passes `grammar` to `complete()`. Grammar/retry policy tunable via `host-config` + request-shaping phases.
6. **Also adopt from Pi:** tool-result `terminate` flag; steering + follow-up injection; `AgentMessage`-superset (custom types in transcript, filtered at LLM boundary).
7. **v1 scope = full set** — intent router, task classifier/routing, tool selector (thin pass-through), context compressor (reworked `manager-context`), permission gate (thin single-rule). *(Fallback reclassified to core mechanism below.)*

### Post-review reconciliation (2026-07-01)

Review surfaced three open points. Resolved:

1. **Dispatch core-native (no `host-hook` import).** Core loop invokes each interceptor's *exported* 
   `interceptor` interface at each phase; enabled/disabled via `jan-klod.yaml` (ordering structural). 
   No `host-hook.wit` capability import — earlier draft contradicted "core drives natively".
2. **Generic hook only for `interceptor-context`.** History trimming internal to `interceptor-context`; 
   loop doesn't call `context-manager` directly. `context-manager.wit` subsumed.
3. **Provider fallback is core mechanism, not interceptor.** Re-issues *same* failed request on next 
   provider (on-provider-error retry = same category as retry/validate) — `prepare-next-turn` can't do it. 
   `interceptor-task-router` does classification + routing only.

### Open Questions

- ~~**Interceptor WIT shape**~~ **Resolved:** [`wit/interceptor.wit`](../../../wit/interceptor.wit) — 
  generic `intercept` over `phase` enum + `subscribed-phases`, return `result<decision, interceptor-error>`. 
  Streaming: preview-vs-authoritative rule — tokens preview; loop emits authoritative at boundary, which 
  `after-response`/`finalize` may `replace`. Interception at turn boundaries.
- ~~**UI gap**~~ **Resolved by `ask`:** interceptor returns `ask(user-prompt)`, loop suspends, routes to driver 
  (TUI/chat/`api-*`), re-invokes with answer. `default-answer` for headless/no-driver. Permission gate never touches UI.
- ~~**Interceptor ordering**~~ **Resolved:** structural (phase enum + load order), not configured. 
  `jan-klod.yaml` only enables/disables.
- **`interceptor-task-router` scope** — one extension (classification + routing) or split? Deferred.
- **Carry-overs:** `whatlang` language detection, grammar format (GBNF vs JSON-schema), token-count strategy.

## Next Steps

Re-architect the [Phase 2 plan](../2026-07-01-phase2-agent-loop/PLAN.md) around
this decision (thin core loop + interceptor framework + the full interceptor set)
and produce an implementation plan. The already-built Slice 2a intent router is
re-cast as the first interceptor.
