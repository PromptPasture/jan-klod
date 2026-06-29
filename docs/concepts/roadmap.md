---
type: concept
title: Roadmap
description: Phased plan from the Rust + Wasmtime + Component Model foundation decision to a shippable, polyglot-extension agent runtime.
tags: [roadmap, planning, rust, wasmtime, component-model, phases]
created: 2026-06-29
updated: 2026-06-29
---

# Roadmap

This is the build sequence implied by the foundation decision
[decisions/2026-06-29-component-model-rust](../decisions/2026-06-29-component-model-rust/Handoff.md).
It is deliberately a **validated-pivot-then-port**: the Go + Wazero MVP proved the
*architecture* end-to-end; we now prove the *foundation* (Component Model on
Rust + Wasmtime) by front-loading the risky check as the first *real* slice (a thin
walking skeleton behind a go/no-go gate), then build out on it.

## Ground rules carried into every phase

- **`core` is Rust, and Rust only.** It is the small, rarely-changing container —
  config loader, extension registry, lifecycle, the Wasmtime component host, event
  bus, observability. Zero agent behaviour. See [Architecture](architecture.md).
- **Extensions are polyglot by design.** Each extension is authored in whatever
  language fits it best — any `wit-bindgen` language, all targeting the same
  [WIT contracts](contracts.md) and interchangeable against them. We switch language
  per extension; Rust is one option among many, not required just because `core` is
  Rust. (This is what the *ecosystem* can do.)
- **Our own built extensions: default Go (TinyGo) or Rust.** For first-party
  extensions we gate the choice by CM-toolchain maturity *and* supply-chain posture,
  not just ecosystem fit — so TS/JS and Python are case-by-case (only when a library
  is decisive + hygiene controls applied) and Kotlin/JVM is excluded for now. The
  build pipeline is not protected by the runtime sandbox, so npm-style supply-chain
  risk weighs on language choice. See
  [decisions/2026-06-29-extension-technologies](../decisions/2026-06-29-extension-technologies/BRAINSTORM.md).
- **The launcher/updater is a tiny Go binary**, separate from `core` so it survives
  a core swap. See [Blue/Green Deployment](blue-green-deployment.md).
- **The WIT contracts already exist** (`wit/*.wit`, 15 interfaces) and are canonical.
  They survived the Go-source reset and are the fixed point everything builds against.
- **All implementation code lives under `src/`** (`src/core/` for the Rust host,
  `src/extensions/<name>/` for guests); `wit/` stays at the repo root as
  language-agnostic contracts. Detailed Phase 1 checklist:
  [PLAN.md](../decisions/2026-06-29-extension-technologies/PLAN.md).
- **Just-in-time library choices.** Every **(TBD)** in [Architecture](architecture.md)
  is resolved at the phase that first needs it (YAGNI), never speculatively.

## Starting point (after the reset)

The Go MVP source has been removed; its findings live in the decision records
([2026-06-28-mvp-wasm-host](../decisions/2026-06-28-mvp-wasm-host/Handoff.md),
[2026-06-28-go-wasm-stack](../decisions/2026-06-28-go-wasm-stack/Handoff.md)). What
remains and carries forward: the **`wit/` contracts**, the architecture/concept docs,
and the validated *behaviours* (config-driven load, lifecycle, host-http, an
OpenAI-compatible provider, an in-memory store) — to be re-implemented as real
Component-Model code, not the hand-rolled JSON ABI.

---

## Status tracker

The single source of truth for where we are. **The loop:** take the first phase not
`done` → run its slices → pass its **exit gate** → set it `done` → repeat. Update the
flag here as state changes (a phase may sit at `blocked` on its gate).

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Phase | Flag | Gate / note |
|---|---|---|
| 1 — Walking skeleton + foundation gate | `in-progress` | **Slice 1a PASSED** (2026-06-29 — CM-in-Rust + TinyGo guest round-trip, async model decided); [verdict](../decisions/2026-06-29-extension-technologies/SLICE-1A-GATE.md). Slice 1b (MVP parity) next |
| 2 — Agent loop | `not-started` | starts after Phase 1 exit gate |
| 3 — Persistence + inbound network | `not-started` | — |
| 4 — Clients & integrations | `not-started` | — |
| 5 — Distribution & ops | `not-started` | — |

Built-extension language assignments and their own status live in the
[Extension Technologies brainstorm](../decisions/2026-06-29-extension-technologies/BRAINSTORM.md#near-term-assignments-provisional--confirmed-at-the-phase-1-gate).

---

## Phase 1 — Walking skeleton + foundation gate

**Goal:** prove the foundation on *real* code (no throwaway spike) and then reach the
Go MVP's parity on it. The risky foundation check is front-loaded as the first slice.

**Slice 1a — the gate (front-load the risk).** The thinnest possible vertical slice:
a Rust + Wasmtime `core` that loads **one thin Go (TinyGo) component** across the
Component-Model boundary and calls it, plus the **async model** decision (`tokio` vs
sync Wasmtime). Keep this component a trivial stub (a `provider` that echoes a single
`complete`) so toolchain friction surfaces on ~10 lines — *not* on the full agent
loop. Go is our chosen default non-Rust extension language (see
[Extension Technologies](../decisions/2026-06-29-extension-technologies/BRAINSTORM.md));
this slice also proves its TinyGo CM toolchain (`wkg` deps, `wasi:cli` world quirk).

- **Go/no-go checkpoint:** if CM-in-Rust is clean and the non-Rust guest works
  end-to-end → continue to 1b. If friction outweighs the payoff → fall back to
  Go + wazero + JSON-ABI with eyes open. *No build-out happens until this passes.*

**Slice 1b — build out to MVP parity.**

- Rust `core`: config loader (`jan-klod.yaml`), extension registry +
  dependency-graph boot ordering, lifecycle (`init → start → stop`/health),
  the Wasmtime **component** host.
- Port host capabilities to the Component Model: `host-log`, `host-config`, `host-http`.
- Re-author `provider-openai` and `store-memory` as real `wit-bindgen` components
  in **Go (TinyGo)** (the default non-Rust extension language).
- Replace the broken Go build targets with Cargo (+ a guest build path per language).

**Exit gate:** config-driven load → lifecycle → an OpenAI-compatible completion
through a sandboxed component, with an in-memory store, all over the Component Model.

## Phase 2 — First real value: the agent loop

**Goal:** the runtime does something useful end-to-end.

- `manager-agent-loop` (Option A — zero behaviour in core) + `manager-context`.
  Both are **extensions, not core**, built in **Go (TinyGo)** — orchestration-heavy
  pure logic, our default non-Rust language. Authoring the largest first-party
  extension outside Rust is also our strongest polyglot proof.
- Small-model harness pieces: intent router, step controller, retry/validate loop —
  see [Small-Model Harness](small-model-harness.md).
- Wire provider fallback and task routing (already specified in [Architecture](architecture.md)).

**Exit gate:** a query runs the full loop against one provider and returns a grounded answer.

## Phase 3 — Persistence + inbound network

**Goal:** durable state and a way for the outside world to reach core.

- `store-sqlite` → resolve the **host-side SQLite (TBD)** (`rusqlite` bundled vs pure).
- Design the **`host-serve`** capability (inbound listener) and build `api-rest`
  (REST + SSE) → resolve the HTTP-framework and SQL **(TBD)**s.
- Resolve **UI ↔ core transport** (current lean: a UI client always connects via `api-rest`).

**Exit gate:** state persists across restart; an external HTTP client drives core via `api-rest`.

## Phase 4 — Clients & integrations

**Goal:** the human- and agent-facing surfaces.

- `jan-klod-ui` client: TUI first (toolkit **TBD**), then GUI by launch flag.
- Design the **`host-socket`** capability + `chat-telegram` first — this unlocks the
  headless Raspberry-Pi / container use case (chat-only access, no UI client).
- `agent-*` ACP delegation, both directions.

## Phase 5 — Distribution & ops

**Goal:** ship it and keep it updatable.

- The tiny **Go launcher/updater**: blue/green stage → flip → health-check → rollback.
- [Configurator](configurator.md) (web ZIP generator) + curated bundles.

---

## Cross-cutting (continuous, not a phase)

- **Observability** — structured logging, Prometheus, OpenTelemetry — wired from Phase 1.
- **Testing** — `cargo test` for core; a WASM-component test harness that loads a
  guest and verifies its WIT interface; integration tests with real SQLite + embedded Wasmtime.
- **Library decisions** — each **(TBD)** in [Architecture](architecture.md) is closed
  at its phase and recorded as a dated decision under `decisions/`.

## Open questions feeding the phases

Tracked in the foundation decision's
[open questions](../decisions/2026-06-29-component-model-rust/Handoff.md#open-questions-carried-forward--new):
non-Rust guest toolchain maturity (Phase 1 — **settled at the Slice 1a gate**:
TinyGo CM toolchain works), `host-serve`/`host-socket` design (Phase 3/4),
UI↔core transport (Phase 3), the Rust async model (Phase 1 — **resolved**: sync
baseline, `tokio` at `host-http`), host-side SQLite library (Phase 3), and
carry-over agent-loop tunables (retry limit, context compression, ACP delegation
timeout — Phase 2).
