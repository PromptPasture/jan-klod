---
type: concept
title: Roadmap
description: Phased plan from the Rust + Wasmtime + Component Model foundation decision to a shippable, polyglot-extension agent runtime, then on to a file-workspace-capable agent (streaming, host-fs/host-process, tool fleet).
tags: [roadmap, planning, rust, wasmtime, component-model, phases]
created: 2026-06-29
updated: 2026-07-02
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
| 3 — Persistence + inbound network | `done` | [PLAN.md](../decisions/2026-07-02-phase3-persistence-network/PLAN.md) (2026-07-02). **Exit gate passed** (`make phase3-gate` in CI): durable state survives a `Runtime` restart (host-side SQLite `Store` via `rusqlite` bundled — persistence is host-side, not SQLite-in-wasm; session transcripts persisted) **and** an external HTTP client drives the loop over the host-side REST surface (`jan_klod_core::serve` on `tiny_http`, launchable via `jan-klod serve`). Boundary calls: store proxied host-side (no store guest); REST surface host-side/sync (not an `api-rest` guest / `axum`). Carried-forward, non-blocking: SSE streaming (with the run-handle), interceptor `host-storage` backed by the shared `Store`. |
| 4 — Clients & integrations | `done` | [PLAN.md](../decisions/2026-07-02-phase4-clients-integrations/PLAN.md) (2026-07-02). **Exit gate passed** (`make phase4-gate` in CI): a UI client (`jan-klod-ui` — line REPL + `ratatui` TUI, a separate process over REST) drives core, and an inbound `chat-telegram` message drives a turn + reply, both offline. `agent-*` ACP delegation via the conductor's `ToolInvoker` seam (`delegate`). Decisions: clients are separate processes over the host-side REST surface; Telegram needs no `host-socket` (outbound HTTP suffices). Carried-forward: GUI (Tauri), concrete ACP-over-HTTP transport + `build_agent` wiring, `host-socket`. |
| 5 — Distribution & ops | `done` | [PLAN.md](../decisions/2026-07-02-phase5-distribution-ops/PLAN.md) (2026-07-02). **Exit gate passed** (`make phase5-gate` in CI): the tiny Go supervisor (`src/supervisor/`, stdlib-only) runs blue/green flip→health→commit/rollback (probes `GET /health`), unit-tested both ways; `make bundle` produces a self-contained archive whose extracted `jan-klod` boots offline. Deploy unit = host-side core binary + provider/interceptor guests. Carried-forward: staging (download + checksum/WIT validation), the bundle matrix, the interactive web Configurator. |
| 6 — Streaming & steering | `done` | [PLAN.md](../decisions/2026-07-02-phase6-streaming-steering/PLAN.md) (2026-07-02). **Exit gate passed** (`make phase6-gate` in CI): the conductor emits events via a push `EventSink` (fits the sync loop) → **SSE** over the REST surface (`tiny_http` streaming, `jan-klod-ui` consumes it live) → **cancel** (a sink `Stop`, incl. client-disconnect) → **steering** (a `Driver::follow_up` injects another cycle at `prepare-next-turn`). Carried-forward: per-token deltas, TUI/Telegram streaming. |
| 7 — File-workspace substrate | `not-started` | The two host-mediated capabilities the sandbox otherwise denies — **`host-fs`** (scoped workspace read/write; COW/checkpoint candidate) and **`host-process`** (spawn + hold a long-lived child: code exec, ssh, lsp/debug/browser). *Nothing file- or execution-shaped can be built until these land.* |
| 8 — Tool fleet | `not-started` | The sandboxed `tool-*` components over the substrates: `host-fs` (read/write/edit/ast-edit/find/grep/ast-grep/checkpoint/git), `host-process` (bash/eval/ssh/job/lsp/debug/browser), `host-http` (fetch; web-search built). Cheap to add once Phase 7 lands. |

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

## Phase 6 — Streaming & steering

**Goal:** the loop streams incrementally, and a driver can interrupt and steer it.
Promotes the Phase 2 carry-forward — the run-handle was prototyped as a core Rust
entry (`build_agent`/`run_with`) but the loop currently returns a whole answer.

- **Run-handle** — `run(session, message)` → a handle; `next-event` streams
  `text-delta` / `tool-invoked` / `tool-result` / `warning` / `done`; `cancel` /
  `close`; a `pending-prompt` event + `provide-answer` resuming the interceptor
  `ask`; a **steering + follow-up-injection** queue. Preview-vs-authoritative
  streaming (see [`wit/interceptor.wit`](../../wit/interceptor.wit) `finalize`).
- **SSE on the REST surface** — `serve` streams `next-event` as Server-Sent Events
  (the deferred `/turn` streaming); the UI client + Telegram consume incrementally.
- **Driver-capability WIT** — promote the core Rust loop-entry to the WIT shape an
  `api-*`/`chat-*` guest would import, if/when those become guests.

**Exit gate:** a driver runs a multi-step turn, receives events incrementally over
SSE, answers an `ask` mid-turn, and cancels a run — offline.

## Phase 7 — File-workspace substrate

**Goal:** give the sandbox **mediated** file and process access — the two
capabilities everything file- or execution-shaped depends on. Neither exists yet by
design (the sandbox grants no filesystem, no process); both are added as host-owned,
routed capabilities.

- **`host-fs`** — a scoped, path-jailed read/write view of a workspace directory.
  Every file-touching tool (read/write, edit, grep/find) needs it. Copy-on-write /
  overlay isolation for safe edits + checkpoint/restore is a candidate model.
- **`host-process`** — spawn and **hold a long-lived child process**. A *co-equal*
  substrate, not a sub-case: **code execution** (`bash`/`eval`) depends on it, as do
  `ssh`, background jobs, and the language-server / debugger / browser bridges.
- **Open** — whether these are two capabilities or one; the isolation model (path
  jail, resource limits, COW). Resolve at this phase and record as a dated decision.

**Exit gate:** a sandboxed extension reads and writes a workspace file through
`host-fs` (jailed to the workspace) and runs a command through `host-process`, both
core-mediated — verified offline.

## Phase 8 — Tool fleet

**Goal:** the ordinary sandboxed `tool-*` components — cheap to add once Phase 7
lands — grouped by the capability they route through. The only shared design work is
routed I/O: file/exec tools go through `host-fs`/`host-process`, never raw OS.

| Routes through | `tool-*` |
|---|---|
| `host-fs` | read, write, edit, ast-edit, find (glob), grep, ast-grep, checkpoint, git |
| `host-process` | **bash/shell**, **eval (code exec)**, ssh, job, lsp, debug (dap), browser |
| `host-http` (have it) | fetch, web-search *(built)* |
| none / local | bm25 local search |

Tools are advertised to the loop at `select-tools` (`interceptor-tool-selector`),
gated at `tool-call` (`interceptor-permission`), and dispatched through the
conductor's `ToolInvoker`. **Deferred / out of tier:** curated-memory tools (the
[open memory question](#curated-memory--open-question-not-a-phase)) and multimodal
(image/tts — need capable providers, off-target for small text models). **Not
tools:** `ask` (interceptor decision), subagent dispatch (`agent-*` delegation),
skills (`registry-skills`), a per-turn watcher/critic and code-review-with-verdict
(`interceptor-*`).

**Exit gate:** the loop completes a multi-step task using at least one `host-fs` tool
(e.g. read + edit a file) and one `host-process` tool (e.g. run a command),
end-to-end.

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

## Curated memory — open question (not a phase)

The file-workspace tier (files, processes, and the `tool-*` fleet) is now scoped as
**Phases 7–8**. One item from that original scope stays **out** of the roadmap as a
deliberate open question: **long-term / curated memory.**

Beyond `store-sqlite` (durable KV/history persistence, delivered in Phase 3), a
coding agent benefits from *curated* memory — working vs episodic recall, semantic
search, consolidation. It is **unresolved whether jan-klod should ship this at all**:
it may belong in a third-party `store-*`/`tool-*` extension, an MCP server via
`registry-mcp`, or an external service, rather than a first-party contract. Decide
*if* before *how*; do not add a memory-curation interface speculatively (YAGNI).
Persistence is done; curation is deliberately parked as a question, and the
memory-tool row is excluded from the Phase 8 fleet accordingly.
