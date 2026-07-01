---
type: concept
title: Roadmap
description: Phased plan from the Rust + Wasmtime + Component Model foundation decision to a shippable, polyglot-extension agent runtime.
tags: [roadmap, planning, rust, wasmtime, component-model, phases]
created: 2026-06-29
updated: 2026-07-01
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
- **Our own built extensions default to Rust.** `core` is Rust and the team is
  Rust-first, so first-party extensions are Rust too — one language means shared
  types, a single CI/lockfile/audit path, and no GC caveat. Another language is used
  only when an ecosystem library or constraint makes it *decisive*, and any such
  non-Rust choice is still gated by CM-toolchain maturity *and* supply-chain posture
  (TS/JS and Python case-by-case with hygiene controls; Kotlin/JVM excluded for now).
  Go (TinyGo) stays fully supported but is no longer the default. The polyglot
  boundary is kept proven and re-runnable by the Slice 1a TinyGo gate (`make gate`) —
  we don't need a production Go extension to hold that guarantee. The build pipeline
  is not protected by the runtime sandbox, so npm-style supply-chain risk weighs on
  any non-Rust language choice. See
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
| 1 — Walking skeleton + foundation gate | `done` | **Slice 1a PASSED** (2026-06-29); [verdict](../decisions/2026-06-29-extension-technologies/SLICE-1A-GATE.md). **Slice 1b done** — `jan-klod-core` boots from `config.yaml` (registry, tier boot order, lifecycle, component host); all three host caps (`host-log`/`host-config`/`host-http`) are real CM imports; three Rust guests (`store-memory` + `provider-openai` + `manager-agent-loop` — the latter retired in Phase 2) build and verify offline; the exit gate runs as one routed turn in the sandboxed agent-loop guest (`tests/routing.rs`); supply-chain CI gates (`cargo-audit`/`cargo-deny`/`govulncheck` + SBOM) wired in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) |
| 2 — Agent loop | `done` | **Re-architected 2026-07-01** ([decision](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md)): thin loop *mechanism* in **core** (`conductor` + `intercept` dispatcher); every decision is a sandboxed **`interceptor-*`** guest the core calls natively. Full v1 set built (`intent-router`, `task-router`, `context`, `tool-selector`, `permission`); `manager-agent-loop`/`agent-loop.wit` retired. **Exit gate passed 2026-07-02** (`make phase2-gate` in CI): intent → shaping → ReAct cycle → provider fallback → grounded answer, plus the integrated permission-`ask`, all through sandboxed guests offline. Carried-forward refinements (streaming run-handle; interceptor-`llm` real routing; `tool-callable` fleet) are non-blocking. Detailed checklist: [PLAN.md](../decisions/2026-07-01-phase2-agent-loop/PLAN.md) |
| 3 — Persistence + inbound network | `in-progress` | Planning recorded 2026-07-02: [PLAN.md](../decisions/2026-07-02-phase3-persistence-network/PLAN.md). Slices: 3a host-side persistent store (`store-sqlite`, `rusqlite` bundled — persistence is host-side, not SQLite-in-wasm), 3b `host-serve` + `api-rest` (REST + SSE over `axum`), 3c UI↔core transport, 3d exit gate. |
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
loop. Go (TinyGo) is the non-Rust language we validate at the gate (see
[Extension Technologies](../decisions/2026-06-29-extension-technologies/BRAINSTORM.md));
this slice proves its TinyGo CM toolchain (`wkg` deps, `wasi:cli` world quirk) and
stays as the standing polyglot canary (`make gate`) even though our own extensions
are now Rust.

- **Go/no-go checkpoint:** if CM-in-Rust is clean and the non-Rust guest works
  end-to-end → continue to 1b. If friction outweighs the payoff → fall back to
  Go + wazero + JSON-ABI with eyes open. *No build-out happens until this passes.*

**Slice 1b — build out to MVP parity.**

- Rust `core`: config loader (`config.yaml`), extension registry +
  dependency-graph boot ordering, lifecycle (`init → start → stop`/health),
  the Wasmtime **component** host.
- Port host capabilities to the Component Model: `host-log`, `host-config`, `host-http`.
- Re-author `provider-openai` and `store-memory` as real `wit-bindgen` components
  in **Rust** (`wasm32-wasip2` target + `wit-bindgen`; no `cargo-component` needed) — the default for our first-party extensions.
- Replace the broken Go build targets with Cargo (+ a guest build path per language).

**Exit gate:** config-driven load → lifecycle → an OpenAI-compatible completion
through a sandboxed component, with an in-memory store, all over the Component Model.

## Phase 2 — First real value: the agent loop

**Goal:** the runtime does something useful end-to-end. **Re-architected 2026-07-01**
([Thin Loop + Interceptor Middleware](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md)).

- **Thin loop mechanism in core (Rust):** `stream → tools → loop`, core-native
  interceptor dispatch (calling each extension's exported `interceptor` interface),
  grammar passthrough, parse/validate/retry-with-correction, **provider fallback**
  (on-provider-error re-issue from the `providers:` list), streaming handles, cancel,
  steering/follow-up queue, tool-result `terminate`. Zero policy. Retires the
  `manager-agent-loop` extension.
- **Interceptor framework + the full v1 set** (all sandboxed Rust `interceptor-*`
  extensions, each exporting `interceptor`): `interceptor-intent-router` (before-loop;
  recast Slice 2a), `interceptor-task-router` (select-model — task classification +
  task→model routing), `interceptor-context` (select-context; the reworked context
  compressor — history/compression handled internally), `interceptor-tool-selector`
  (select-tools; built thin), and `interceptor-permission` (tool-call; built thin).
- **Core mechanism, tunable via seams:** constrained-decoding grammar + retry/validate
  stay fixed in the loop; construction/policy tuned via `host-config` + the
  request-shaping phases — see [Small-Model Harness](small-model-harness.md).

**Exit gate:** a query runs the full loop (intent hook → shaped request → ReAct cycle
→ fallback on a simulated provider failure) against one provider and returns a grounded
answer, driven end-to-end through the core-exposed loop entry.

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

## File-workspace tier (not yet scoped)

The runtime today can *reason and call tools*, but it cannot yet *work with files* —
the sandbox grants no filesystem access. Three gaps stand between the current design
and an agent that can operate on a workspace of files (coding is one use of this, not
the only one). All are **post-v1** and recorded here as scope, not commitments.

1. **Two substrate capabilities.** The sandbox grants no filesystem and no process
   access by design, so both must be added as host-mediated, routed capabilities:
   - **`host-fs`** — a scoped, read/write view of a workspace directory. Every
     file-touching tool (read/write, edit, grep/find) needs it. (Copy-on-write /
     overlay isolation for safe edits + checkpoint/restore is a candidate model.)
   - **`host-process`** — spawn and **hold a long-lived child process**. This is a
     *co-equal* substrate, not a sub-case: the single most important agent tool —
     **code execution** (`bash`/`eval`) — depends on it, as do `ssh`, background
     jobs, and the language-server / debugger / browser bridges.

   Open question: whether these are two capabilities or one; either way, *nothing
   file- or execution-shaped can be built as a sandboxed extension until they land.*

2. **Long-term / curated memory — open, and maybe not ours.** Beyond `store-sqlite`
   (durable KV/history persistence, already planned), a coding agent benefits from
   *curated* memory: working vs episodic recall, semantic search, consolidation. It is
   **unresolved whether jan-klod should ship this at all** — it may belong in a
   third-party `store-*`/`tool-*` extension, an MCP server via `registry-mcp`, or an
   external service, rather than a first-party contract. Decide *if* before *how*; do
   not add a memory-curation interface speculatively (YAGNI). Persistence is planned;
   curation is deliberately parked as a question.

3. **A fleet of `tool-*` extensions.** All ordinary sandboxed `tool-*` components,
   cheap to add *once #1 exists*, grouped by the capability they route through:

   | Needs | Candidate `tool-*` |
   |---|---|
   | `host-fs` | read, write, edit, ast-edit, find (glob), grep, ast-grep, checkpoint, git |
   | `host-process` | **bash/shell**, **eval (code exec)**, ssh, job, lsp, debug (dap), browser |
   | `host-http` (have it) | web-search *(built)*, fetch |
   | none / local | bm25 local search |

   The only shared design work is routed I/O — file/exec tools go through
   `host-fs`/`host-process`, never raw OS. **Deferred / out of this tier:** curated
   memory tools (recall/retain — the [open memory question](#file-workspace-tier-not-yet-scoped)),
   and multimodal tools (image/tts — need capable providers, off-target for small text
   models). **Not tools:** `ask` is the interceptor `ask` decision; subagent dispatch
   is `agent-*` delegation; skills are `registry-skills`; a per-turn *watcher/critic*
   and *code-review-with-verdict* are `interceptor-*` (mechanism already supports them,
   just not in the v1 set).
