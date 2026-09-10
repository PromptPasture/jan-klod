---
type: concept
title: Roadmap
description: Phased plan from the Rust + Wasmtime + Component Model foundation decision to a shippable, polyglot-extension agent runtime (v0.1.0, Phases 1–12), then on to the Harness-as-a-Platform vision — client protocol, event-sourced session log, OS-level effect sandbox, capability manifest + signed registry, web client + GUI shell, MCP/ACP ports (Phases 13–18).
tags: [roadmap, planning, rust, wasmtime, component-model, phases, vision]
created: 2026-06-29
updated: 2026-09-09
status: v0.1.0 complete (Phases 1–12 done, nothing tagged or released yet); Harness as a Platform under way — Phases 13 and 14 done (13c deferred to Phase 17), 15 and 16 in progress, 17–18 not-started
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
| 1 — Walking skeleton + foundation gate | `done` | **Slice 1a PASSED** (2026-06-29); [verdict](../decisions/2026-06-29-extension-technologies/SLICE-1A-GATE.md). **Slice 1b done** — `jan-klod-core` boots from `config.yaml` (registry, tier boot order, lifecycle, component host); all three host caps (`host-log`/`host-config`/`host-http`) are real CM imports; three Rust guests (`store-memory` + `provider-openai` + `manager-agent-loop` — the latter retired in Phase 2) build and verify offline; the exit gate runs as one routed turn in the sandboxed agent-loop guest (`tests/routing.rs`); supply-chain CI gates (`cargo-audit`/`cargo-deny`/`govulncheck` + SBOM via `cargo-cyclonedx`) wired in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) |
| 2 — Agent loop | `done` | **Re-architected 2026-07-01** ([decision](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md)): thin loop *mechanism* in **core** (`conductor` + `intercept` dispatcher); every decision is a sandboxed **`interceptor-*`** guest the core calls natively. Full v1 set built (`intent-router`, `task-router`, `context`, `tool-selector`, `permission`); `manager-agent-loop`/`agent-loop.wit` retired. **Exit gate passed 2026-07-02** (`make gate` in CI): intent → shaping → ReAct cycle → provider fallback → grounded answer, plus the integrated permission-`ask`, all through sandboxed guests offline. Carried-forward refinements (streaming run-handle; interceptor-`llm` real routing; `tool-callable` fleet) are non-blocking. Detailed checklist: [PLAN.md](../decisions/2026-07-01-phase2-agent-loop/PLAN.md) |
| 3 — Persistence + inbound network | `done` | [PLAN.md](../decisions/2026-07-02-phase3-persistence-network/PLAN.md) (2026-07-02). **Exit gate passed** (`make gate` in CI): durable state survives a `Runtime` restart (host-side SQLite `Store` via `rusqlite` bundled — persistence is host-side, not SQLite-in-wasm; session transcripts persisted) **and** an external HTTP client drives the loop over the host-side REST surface (`jan_klod_core::serve` on `tiny_http`, launchable via `jan-klod serve`). Boundary calls: store proxied host-side (no store guest); REST surface host-side/sync (not an `api-rest` guest / `axum`). Carried-forward, non-blocking: SSE streaming (with the run-handle), interceptor `host-storage` backed by the shared `Store`. |
| 4 — Clients & integrations | `done` | [PLAN.md](../decisions/2026-07-02-phase4-clients-integrations/PLAN.md) (2026-07-02). **Exit gate passed** (`make gate` in CI): a UI client (`jan-klod-ui` — line REPL + `ratatui` TUI, a separate process over REST) drives core, and an inbound `chat-telegram` message drives a turn + reply, both offline. `agent-*` ACP delegation via the conductor's `ToolInvoker` seam (`delegate`). Decisions: clients are separate processes over the host-side REST surface; Telegram needs no `host-socket` (outbound HTTP suffices). Carried-forward: GUI (Tauri), concrete ACP-over-HTTP transport + `build_agent` wiring, `host-socket`. **Extended 2026-08-09:** both chat surfaces can now *ask* — the REST/SSE driver serves its own socket while a turn is blocked, and Telegram's `ChatDriver` asks in the chat and takes the user's next message as the answer (deferring other chats' messages so the poll offset cannot lose them). Until then every confirmation on both surfaces was auto-denied by the headless driver. |
| 5 — Distribution & ops | `done` | [PLAN.md](../decisions/2026-07-02-phase5-distribution-ops/PLAN.md) (2026-07-02). **Exit gate passed** (`make gate` in CI): the tiny Go supervisor (`src/supervisor/`, stdlib-only) runs blue/green flip→health→commit/rollback (probes `GET /health`), unit-tested both ways; `make bundle` produces a self-contained archive whose extracted `jan-klod` boots offline. Deploy unit = host-side core binary + provider/interceptor guests. Carried-forward: staging (download + checksum/WIT validation), the bundle matrix, the interactive web Configurator. |
| 6 — Streaming & steering | `done` | [PLAN.md](../decisions/2026-07-02-phase6-streaming-steering/PLAN.md) (2026-07-02). **Exit gate passed** (`make gate` in CI): the conductor emits events via a push `EventSink` (fits the sync loop) → **SSE** over the REST surface (`tiny_http` streaming, `jan-klod-ui` consumes it live) → **cancel** (a sink `Stop`, incl. client-disconnect) → **steering** (a `Driver::follow_up` injects another cycle at `prepare-next-turn`). Carried-forward: per-token deltas, TUI/Telegram streaming. |
| 7 — File-workspace substrate | `done` | [PLAN.md](../decisions/2026-07-02-phase7-file-workspace-substrate/PLAN.md) (2026-07-02). **Exit gate passed** (`make gate` in CI): **`host-fs`** (path-jailed workspace read/write; `..`/absolute/no-workspace denied) and **`host-process`** (bounded run-to-completion exec — workspace-jailed cwd, timeout, output cap; disabled denied), both unit-tested host-side and driven **across the CM boundary** by probe guests (`tool-fs-probe` — since retired, its tests retargeted to `tool-fs`; `tool-proc-probe`) through `tool_host::ToolExtension`. Default-deny + workspace-jailed. Carried-forward: symlink hardening/COW, long-lived children/OS isolation, wiring the substrates into the loop's tools. |
| 8 — Tool fleet | `done` | [PLAN.md](../decisions/2026-07-02-phase8-tool-fleet/PLAN.md) (2026-07-02). **Exit gate passed** (`make gate` in CI): a model tool call runs through the whole loop — advertised at `select-tools`, gated at `tool-call` (permission ask→approve), dispatched by the `ToolFleet` (`conductor::ToolInvoker`) to a real tool that writes through `host-fs`, result fed back, grounded answer. Fleet: `tool-fs` (a single host-fs tool with read/write/grep ops), `tool-shell` (host-process); `build_agent` instantiates enabled `tool.*` with the default-deny substrates from config. The rest of the fleet follows the same pattern. **Extended 2026-08-08:** `tool-edit` — hash-anchored partial edits (`view` → `replace`/`insert`), the [edit-reliability lever](small-model-harness.md#edit-reliability-tool-edit--built) the harness specifies; a stale anchor is rejected with nothing written. `tool.fs`/`tool.edit` + `interceptor.tool-selector`/`permission` are now **enabled in the shipped `config.yaml`**, so a fresh install can read and edit its workspace (behind the confirmation gate) without hand-editing config. Also `tool-find` — bounded glob discovery (guest-side walk over `host-fs` `list-dir`; visit/depth/result/byte caps, heavy dirs pruned unless named, a bound that bites is reported), which closes the gap that left `read`/`grep`/`edit` dependent on paths the model had to guess — and `tool-fs`'s `grep` now searches the whole tree on that same walk (optional `glob` filter, `path:lineno:line` hits, match cap), so locating a symbol is one call. The shared matcher/walk/cap logic lives in the `guest-fs` library crate (a workspace member, not a component). **`tool-git`** (read-only: status/diff/log/show/branch) demonstrates the fleet's shaping principle — the guest builds its own argv from a closed op set, so the write half of git is *not expressible* rather than merely disallowed, and every call hardens against repo-supplied `core.fsmonitor`/`hooksPath`/diff-driver execution. It needs the `execution:` substrate but grants far less than `tool-shell`. |
| 9 — Anthropic provider | `done` | **Done 2026-07-03.** `provider-anthropic` guest (Rust, `wasm32-wasip2`): native Anthropic Messages API (`/v1/messages`), system-message extraction to top-level field, `tool_use` block → `ToolCallRequest`, stop-reason normalisation, auth/rate-limit error mapping. Config: `extensions.provider.anthropic.enabled: true`. |
| 10 — Skills + MCP registry | `done` | **Done 2026-07-03.** `registry-skills` (scans `.agents/skills/*.md`, parses YAML frontmatter `name:`/`description:`, exposes via `skill-registry` WIT, `invoke` renders template); `registry-mcp` (SSE/streamable-HTTP MCP gateway, JSON-RPC `tools/list` + `tools/call`). `registry_host.rs` binds both worlds; `CombinedFleet` dispatches tool calls to `ToolFleet` then `RegistryFleet`. `host-fs` added to `skill-registry-world`. |
| 11 — UX polish | `done` | **Done 2026-07-03.** REST surface migrated to resource model (`POST /turn` retired; `GET /sessions`, `POST /sessions`, `GET /session/:id`, `POST /session/:id/message` added); `store::list_namespaces` + `AgentSession::list_sessions`; workspace auto-detection (defaults to `$PWD` when `workspace:` key absent); per-token streaming in TUI via mpsc channel + `apply_delta`/`finish_turn`. UI client and integration tests updated. |
| 12 — Release: GitHub + web | `done` | **Done 2026-07-03.** GitHub Actions release workflow (`.github/workflows/release.yml`; tag `v*` → matrix linux/darwin × x86_64/arm64 bundles + SHA256SUMS, `gh release create`); `scripts/install.sh` (OS/arch detect, checksum verify, installs to `~/.local/bin`); `pages/index.html` (GitHub Pages landing); `docs/quickstart.md`; README rewrite. |
| 13 — Client protocol | `done` | [#35](https://github.com/PromptPasture/jan-klod/issues/35). [Vision](../decisions/2026-09-08-harness-platform-vision/Vision.md) decision 1. **13a done 2026-09-09** ([#41](https://github.com/PromptPasture/jan-klod/issues/41)): `jan-klod-protocol` crate — 8 commands, 8 notifications, `PROTOCOL_VERSION`, a committed JSON Schema with a drift test, and a compatibility test proving the SSE projection loses nothing. **13b done 2026-09-10** ([#42](https://github.com/PromptPasture/jan-klod/issues/42)): `jan-klod-gateway rpc` on stdin/stdout, and `jan-klod` uses it by default — no port, no token, nothing left running. The framing moved into the contract crate (the core writes frames and every client reads them); `turn/follow-up` works over stdio and cannot over REST. **The exit gate is met** — a full turn streams, an `ask` is answered on the same pipe, a `turn/cancel` stops a turn (proven by the completion it never asks for), and REST + SSE still pass. **13c (WebSocket) is deferred until Phase 17 needs it** ([#43](https://github.com/PromptPasture/jan-klod/issues/43)) — `tiny_http` cannot hand back a socket that both times out a read and reads while writing, and the client that wants one is not started, so the socket decision belongs to whoever will use it. |
| 14 — Event-sourced session log | `done` | [#36](https://github.com/PromptPasture/jan-klod/issues/36). Vision decision 3. **Exit gate passed 2026-09-09** (`make gate`): the append-only `events` table with a versioned envelope (#44), and transcript/resume/fork as projections of it (#45) — a session resumed after a restart rebuilds from the log, and a fork at seq *N* runs independently. Nothing writes a transcript except through events; a pre-log database is converted at boot. Gate: after a restart, a resumed session's transcript is rebuilt from the event log and equals the pre-restart transcript; a fork from event *N* runs independently. |
| 15 — OS-level effect sandbox | `in-progress` | [#37](https://github.com/PromptPasture/jan-klod/issues/37). Vision decision 2. **15a done 2026-09-09** ([#46](https://github.com/PromptPasture/jan-klod/issues/46)): `execution.sandbox` policy, the `SandboxBackend` seam, boot-time resolution that never downgrades quietly, and `require: true` denying execution rather than degrading. **15b done 2026-09-10** ([#47](https://github.com/PromptPasture/jan-klod/issues/47)): macOS commands run under a generated Seatbelt profile, so a write outside `writable` is refused by the kernel rather than by a prompt — and `require: true` now permits commands there instead of denying them. Linux is still approval-only until 15c. The per-turn warning is deferred to 16a. Half the gate is met: the macOS half is tested (with an unconfined control for every case, since a command that failed for another reason looks identical to a denial) and the Linux half needs Landlock. **Nothing here is CI-verified** — these tests are macOS-only and CI is Linux ([#95](https://github.com/PromptPasture/jan-klod/issues/95)). Gate: a `tool-shell` command writing outside the workspace is denied on macOS (Seatbelt) and Linux (Landlock); elsewhere the run reports **approval-only** at boot and in the turn; the security-model row cites the tests. |
| 16 — Capability manifest + signed registry | `in-progress` | [#38](https://github.com/PromptPasture/jan-klod/issues/38). Vision decision 4. **16a done 2026-09-10** (#86, #87): every guest ships a manifest generated from its own imports, and the host refuses a component whose manifest is absent, under-declares what it imports, or names an incompatible interface version — cross-validated by two independent readers of the same artifacts. Remaining: 16b (versioning policy), 16c (`ext install` provenance), 16d (registry index). Gate: a component whose manifest omits a capability it imports is refused at boot; a tampered download is refused by `ext install`; an install from a static index fixture works offline; WIT `api-version` mismatch is a clear error. |
| 17 — Web client + GUI shell | `not-started` | [#39](https://github.com/PromptPasture/jan-klod/issues/39). Vision decision 5. Needs 13. Gate: a browser and a Tauri window drive a turn with `ask` + cancel from one front-end codebase served by the core. |
| 18 — Ecosystem ports | `not-started` | [#40](https://github.com/PromptPasture/jan-klod/issues/40). Vision decision 6. Needs 13. Gate: an ACP client fixture runs a turn against the core; an MCP client lists and calls a core-exposed tool — both offline. |

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
stays as the polyglot canary even though our own extensions are now Rust. It is
checked by `host/tests/it/polyglot.rs`: the committed Go-built component is loaded
and called on every `make gate` run, and rebuilt from source wherever `tinygo`
and `wkg` are installed. Until 2026-08-11 this page said `make gate` ran it and
`make gate` did not — the spike was an example someone had to invoke by hand.

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

- Storage → resolved: **host-side SQLite** (`rusqlite`, bundled). Not a component; see [contracts](contracts.md).
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
| `host-fs` | read *(built)*, write *(built)*, grep *(built, tree-wide)*, **edit** *(built)*, **find/glob** *(built)*, ast-edit, ast-grep, checkpoint |
| `host-process` | **bash/shell** *(built)*, **git** *(built, read-only)*, **eval (code exec)**, ssh, job, lsp, debug (dap), browser |
| `host-http` (have it) | **fetch** *(built)*, web-search |
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

## Phase 9 — Anthropic provider

**Goal:** Claude works natively without an OpenAI-compat proxy.

- **`provider-anthropic` extension** — Rust guest implementing `llm-provider`. Calls
  the Anthropic Messages API (`/v1/messages`) via `host-http`. Handles native SSE
  streaming → `completion-chunk` sequence; `tool_use` content blocks → `tool-call-request`
  chunks; error mapping (`401/403` → `auth-failed`, `429` → `rate-limited`, else
  `transient`). `init` reads `api-key` + `model` from `host-config`. Extended thinking
  passthrough via config flag.
- Config: `extensions.provider.anthropic: {enabled: true, api-key: ${ANTHROPIC_API_KEY},
  model: claude-sonnet-4-6}`.

**Exit gate:** enable `provider.anthropic` in `config.yaml` and run `make probe` with
a live `ANTHROPIC_API_KEY`. Offline: canned `host-http` reply → correct chunk sequence.
(`make probe` runs against the enabled provider in config; no `PROVIDER=` flag exists.)

## Phase 10 — Skills + MCP registry

**Goal:** named workflow shortcuts and ecosystem tool access.

- **`registry-skills`** — implements `skill-registry` (`wit/skill-registry.wit`).
  Scans `.agents/skills/` for Markdown files with a YAML `name:` field; exposes them
  via `list-skills` / `invoke` / `reload`. The `ToolFleet` calls `list-skills` at
  `select-tools` and `invoke` at dispatch — the same seam as `tool-callable` but
  through the `skill-registry` interface.
- **`registry-mcp`** — implements `mcp-registry` (`wit/mcp-registry.wit`). Connects
  to configured MCP servers via **SSE only** (via `host-http`; stdio deferred —
  requires long-lived `host-process` carry-forward from Phase 7). Exposes
  `list-tools` / `invoke-tool` / `reconnect`. **Correction (2026-08-09):** the
  guest never called `host-event`, and the interface has since narrowed to
  `publish` — crash *notification* (something reacting) needs a consumer that
  does not exist yet. `ToolFleet` calls `list-tools` at `select-tools` and `invoke-tool`
  at dispatch. Permission gate fires on each outbound MCP call.
- **Prerequisite:** verify `host-event` is granted to `mcp-registry-world` guests
  before starting `registry-mcp`.

**Exit gate:** model calls an MCP tool via a canned SSE stub (offline); model invokes
a skill from `.agents/skills/review.md` and the template is injected correctly.

## Phase 11 — UX polish

**Goal:** the daily-use experience is complete. Per-token streaming has no dependency
on Phases 9–10 and can start immediately in parallel with them.

- **Per-token streaming in TUI** (Phase 6 carry-forward) — `jan-klod-ui`'s `ratatui`
  TUI consumes `text-delta` SSE events incrementally, appending to the active message
  buffer and re-rendering on each event. Wiring only; no architectural change.
- **Session list + resume** — `GET /sessions` returns past sessions from SQLite (id,
  created, preview). `jan-klod-ui --session <id>` or `/sessions` REPL command picks
  a session to resume. `POST /turn` with an existing session id replays stored history
  into the context interceptor before the first new turn.
- **Workspace auto-detection** — when `jan-klod serve` is launched without an explicit
  `workspace:` config key, default to `$PWD`. Config-defaulting change in `Runtime::boot`.

**Exit gate:** launch in a repo, send a message, disconnect, relaunch, resume by id,
receive per-token streaming output in the TUI.

## Phase 12 — Release: GitHub + web

**Goal:** a developer unfamiliar with Jan-Klod can find it, install it, and have a
working session within 15 minutes.

- **GitHub releases** — GitHub Actions workflow: on `git tag v*`, build
  `dist/jan-klod-<version>-<os>-<arch>.tar.gz` for the matrix (linux-amd64,
  linux-arm64, darwin-arm64, darwin-amd64) and publish as release assets. `make bundle`
  already produces the archive; this wires it into CI.
- **Install script** — `scripts/install.sh`: detects OS/arch, downloads the matching
  bundle from the latest GitHub release, verifies checksum, extracts to
  `~/.local/bin/jan-klod`. Stdlib sh, no dependencies. Usage:
  `curl -sSL https://raw.githubusercontent.com/PromptPasture/jan-klod/main/scripts/install.sh | sh`.
- **GitHub Pages site** (`pages/`) — static site served from `pages/` on `main`.
  Content: what Jan-Klod is, the WASM sandboxing differentiator, install command,
  quickstart link. Plain HTML + minimal CSS; no build framework.
- **Quickstart doc** (`docs/quickstart.md`) — install → set API key → `jan-klod serve`
  → first TUI session → fix a bug in a real repo. Under 500 words; golden path only.
- **README rewrite** — what it is (one sentence), differentiator vs Pi (one sentence),
  install, quickstart link, TUI screenshot.

**Exit gate:** cold-start install from the README command completes and the quickstart
produces a model-driven file edit in under 15 minutes.

---

## Post-v0.1: Harness as a Platform (Phases 13–18)

[Vision — Harness as a Platform](../decisions/2026-09-08-harness-platform-vision/Vision.md)
(2026-09-08) reframes jan-klod as an **agent runtime** — kernel + distributions +
clients — and records six decisions. They are phased below by dependency: the
protocol (13) is what the web client (17) and the ecosystem ports (18) build on;
the event log (14) is host-only and independent; the sandbox (15) and the
registry (16) are independent of everything and can run in parallel with 13–14.
Work items are GitHub Issues: one umbrella per phase ([#35](https://github.com/PromptPasture/jan-klod/issues/35), [#36](https://github.com/PromptPasture/jan-klod/issues/36), [#37](https://github.com/PromptPasture/jan-klod/issues/37), [#38](https://github.com/PromptPasture/jan-klod/issues/38), [#39](https://github.com/PromptPasture/jan-klod/issues/39), [#40](https://github.com/PromptPasture/jan-klod/issues/40)), one issue per slice; cross-cutting items are [#59](https://github.com/PromptPasture/jan-klod/issues/59), [#60](https://github.com/PromptPasture/jan-klod/issues/60), [#61](https://github.com/PromptPasture/jan-klod/issues/61), [#62](https://github.com/PromptPasture/jan-klod/issues/62), [#63](https://github.com/PromptPasture/jan-klod/issues/63).
Library choices are still made just-in-time inside each phase.

## Phase 13 — Client protocol

**Goal:** the client surface becomes a contract of the same rank as WIT — its own
versioned schema, its own compatibility tests — so TUI, web, GUI, editors and
scripts share one wire format. Supersedes the Phase 3 lean "UI always connects via
REST"; REST + SSE stay as one *projection* of the protocol.

- **13a — Protocol crate + schema. Done 2026-09-09** ([#41](https://github.com/PromptPasture/jan-klod/issues/41)).
  `jan-klod-protocol` in the core workspace: **eight** commands — the five planned
  here plus `session/get` and `protocol/hello`, which this bullet had omitted, and
  `turn/follow-up` — and **eight** notifications: the conductor's five `Event`
  variants (`text-delta`, `tool-invoked`, `tool-result`, `warning`, `done`) plus
  `ask`, `session/updated`, and `error`, which was also missing here and which the
  SSE surface has emitted all along. `PROTOCOL_VERSION`, a committed JSON Schema
  with a drift test, and a compatibility test asserting every SSE frame's payload
  reaches a notification unchanged. The full list is in
  [Contracts → UI ↔ core](contracts.md#ui--core-client-surface). Resolves the open
  question "own schema vs. ACP wholesale": own schema, ACP as an adapter (Phase 18).
- **13b — stdio JSON-RPC transport. Done 2026-09-10** ([#42](https://github.com/PromptPasture/jan-klod/issues/42)).
  `jan-klod-gateway rpc` speaks the protocol on stdin/stdout (the Codex
  `app-server` / LSP shape) and `jan-klod` uses it **by default**, spawning the
  gateway rather than connecting to one: no port, no token, nothing left running.
  Three things this bullet did not anticipate:
  - **The framing moved into the contract crate**, where the bullet assumed each
    transport would own its own. It has to: the core writes frames and every
    client reads them, and the TUI client depends on neither the core nor
    Wasmtime by design, so a frame type reachable only from the core would have
    been hand-rolled twice. See
    [Contracts](contracts.md#ui--core-client-surface).
  - **`turn/follow-up` works here first.** `Driver::follow_up` has existed since
    the conductor did and REST has no way to deliver a message into a running
    turn; a pipe does.
  - **The handshake is mandatory**, not offered. A negotiation a client can skip
    negotiates nothing.
  The `ask`/answer round-trip is reused rather than duplicated — and came out
  simpler, because a closed pipe is a real signal where a dead socket needs a
  heartbeat to discover. `--addr <host:port>` still drives a running gateway over
  REST + SSE.
- **13c — WebSocket transport. Deferred until Phase 17 needs it**
  ([#43](https://github.com/PromptPasture/jan-klod/issues/43)). The same protocol
  over WebSocket, for browser clients — and the obstacle turned out to be the
  socket rather than the protocol. A parked `ask` has to be able to give up
  (`JK_ANSWER_TIMEOUT_SECS`; the core is single-threaded, so a silent client
  wedges the runtime, not just its own connection) and a mid-turn `turn/cancel`
  has to be readable while frames are being written. `tiny_http` hands an
  upgraded connection back with both halves fused, no `try_clone` and no read
  timeout, so serving this on the existing listener cannot do either. The
  alternatives — a second listener, or replacing the HTTP surface — are
  architecture decisions whose cost only the client that needs the socket can
  justify, and 17a is not started. Measurements worth keeping: `tungstenite`
  costs 6 packages without its `handshake` feature and ~10 with it; a hand-rolled
  RFC 6455 handshake costs `sha1` alone, 1 package.

**Exit gate:** `jan-klod-ui` drives a full turn — streaming, `ask`, cancel — over
stdio JSON-RPC; REST + SSE tests still pass; protocol version negotiated at connect.

## Phase 14 — Event-sourced session log

**Goal:** the session's canonical record is the event stream the clients already
consume, not a transcript. Transcript, `GET /session/:id`, and SSE become
projections; resume, fork, replay and audit fall out.

- **14a — Event table + writer sink. Done 2026-09-09** ([#44](https://github.com/PromptPasture/jan-klod/issues/44)).
  The append-only `events` table (`session`, `seq`, `ts`, `kind`, `payload`,
  keyed on `(session, seq)`) in the host-side SQLite `Store`; `PersistingSink`
  fans out every conductor `Event` to the log without taking the stream from the
  SSE/TUI sinks, and `PersistingDriver` records the `ask`, its answer and any
  steering follow-up — the last of these beyond what this bullet asked for,
  because a steered turn cannot be rebuilt without it. The user message is
  logged first, so a session's log opens with its own input. The permission
  decision needs no kind of its own: a denial is the `Warning` the conductor
  already emits, an approval is the `ask`/`answer` pair. `EVENT_LOG_VERSION`
  is independent of the protocol version. Wired in `run_and_persist`, the one
  funnel all eight turn entry points share. Text deltas are not coalesced.
  See [Architecture → Storage](architecture.md#storage).
- **14b — Projections + resume + fork. Done 2026-09-09** ([#45](https://github.com/PromptPasture/jan-klod/issues/45)).
  `core::projection::transcript` is a pure function over log rows; `replay`,
  `AgentSession::transcript`, `list_sessions`, `GET /session/:id` (now serving
  `messages`, not `turns`) and `GET /sessions` all read through it.
  `POST /session/:id/fork` copies a prefix into a new session that then
  diverges, with `session/fork` in the protocol crate as contract-only until a
  transport lands. The `entries` transcript write is **gone**, and a one-shot
  conversion at boot turns pre-log databases into events — required, not
  optional, since the read surfaces no longer look at `entries`.

**Exit gate:** after a restart, a resumed session's transcript rebuilt from the log
equals the pre-restart transcript; a fork from event *N* runs independently.

## Phase 15 — OS-level effect sandbox

**Goal:** confine what a `host-process` command *does*, not only who may call it.
Closes the Phase 7 "OS isolation" carry-forward and the
[security-model gap](security-model.md#known-gaps).

- **15a — Policy object + approval-only mode. Done 2026-09-09** ([#46](https://github.com/PromptPasture/jan-klod/issues/46)).
  `execution.sandbox` in `config.yaml`: `mode: os | approval-only`,
  `writable: [paths]`, `network: bool`, plus `require: bool` — which denies
  `host-process` outright rather than degrading, for an operator who would rather
  run no command than an unconfined one. `core::sandbox` holds the policy, the
  `SandboxBackend` trait and `NoBackend`; boot resolves the effective mode and
  prints the reason whenever it is not the one requested. No backend on any
  platform yet, so every platform is approval-only (15b changed that for macOS).
  **The per-turn warning is deferred to 16a**: nothing distinguishes a tool that
  uses `host-process` from one that only reads files, so it would fire on every
  tool-using turn. 16a's component-import introspection answers that exactly.
- **15b — macOS Seatbelt backend. Done 2026-09-10** ([#47](https://github.com/PromptPasture/jan-klod/issues/47)).
  A generated `sandbox-exec` profile — `(deny default)`, reads allowed, writes
  only under `writable`, network per policy — applied by rebuilding every command
  as `sandbox-exec -p <profile> -- <command>`. Three things this bullet did not
  anticipate:
  - **Seatbelt matches the *resolved* path**, and getting that wrong looks
    exactly like the sandbox working. A profile granting `/tmp/x` denies a write
    to `/tmp/x/ok`, because `/tmp` is a symlink to `/private/tmp` — with
    "Operation not permitted". Every test's temp directory is under a symlink, so
    an uncanonicalized profile would have passed a suite asserting escapes are
    denied *while denying every grant too*. The in-workspace-write test is what
    catches that class, and it is the one test that a missing sandbox does not
    trip.
  - **15a's `SandboxBackend` could not express a wrapping mechanism.** It took
    `&mut Command`, and a `Command`'s program cannot be changed — only read. It
    now takes and returns an owned command, which fits both a wrapper (Seatbelt)
    and an in-process mechanism (Landlock).
  - **A path that cannot be written into a profile literally is refused, not
    escaped.** SBPL is s-expressions, so a directory name containing `"` could
    close the literal and have the rest read as policy.
  On macOS `require: true` now *permits* commands rather than denying them, since
  there is finally something to require. **Not verified by CI** — the tests are
  macOS-only and CI is Linux
  ([#95](https://github.com/PromptPasture/jan-klod/issues/95)).
- **15c — Linux Landlock backend.** Landlock filesystem rules (+ seccomp for
  network where Landlock cannot express it); graceful fallback to approval-only on
  kernels without Landlock.
- **15d — Windows spike.** Feasibility of a restricted token / AppContainer for a
  spawned command; outcome recorded as a dated decision; approval-only stays the
  Windows default until a backend exists.

**Exit gate:** a `tool-shell` command writing outside the workspace is denied on
macOS and Linux; elsewhere the run reports approval-only; the security-model row
for `host-process` cites the new tests.

## Phase 16 — Capability manifest + signed registry

**Goal:** an extension declares what it needs before it is loaded, the host
cross-checks the declaration against the component's real imports, and installs
are verified. Extends the Phase 5 "staging" carry-forward; prerequisite for a
public extension ecosystem.

- **16a — Manifest + boot cross-check.** Split in two ([#50](https://github.com/PromptPasture/jan-klod/issues/50)),
  because the refusal cannot land before the manifests exist without breaking the
  boot of every shipped guest at once.
  - **16a-1 — the manifest format. Done 2026-09-09** ([#86](https://github.com/PromptPasture/jan-klod/issues/86)).
    `ext/<name>.manifest.toml` beside each `.wasm`: `name`, `version`,
    `api-version`, `kind`, `description`, and `capabilities` read from the
    component's own imports rather than written by its author, so a manifest
    cannot claim less than the artifact beside it. Generated by
    `make -C src/extensions manifests`, not committed — `ext/` is build output,
    and a committed manifest there would describe a component a bare clone does
    not contain. Nothing reads one yet.
  - **16a-2 — the boot cross-check. Done 2026-09-10** ([#87](https://github.com/PromptPasture/jan-klod/issues/87)).
    `Runtime::boot` reads each component's real imports through wasmtime's
    component-type API and refuses three things: a capability imported but not
    declared (naming the interface), a component with no manifest at all, and
    one built against an incompatible `jan-klod:interfaces` version (naming
    both). Top-level `allow-unmanifested: true` is the named widening — *not*
    `extensions.allow-unmanifested`, since every key there must be a category of
    named instances. Over-declaring is deliberately allowed and grants nothing.
    `make bundle` and the installed-layout fixture carry manifests, so a release
    installs and boots with the refusal live. The per-extension import set is
    kept, which is the fact
    [#46](https://github.com/PromptPasture/jan-klod/issues/46)'s deferred
    per-turn sandbox warning was waiting for.
- **16b — WIT versioning policy.** Split three ways
  ([#51](https://github.com/PromptPasture/jan-klod/issues/51)), because the
  refusal already landed with 16a and the adaptation has nothing to adapt yet.
  - **16b-1 — the rules, written down. Done 2026-09-10** ([#88](https://github.com/PromptPasture/jan-klod/issues/88)).
    [Contracts → Versioning](contracts.md#versioning) states what counts as a
    major, minor or patch change to `wit/`, where the version lives, and why
    **pre-1.0 is stricter** rather than laxer: a `0.x` version carries no
    compatibility promise, so a differing minor is refused. Two changes that
    look additive and are not — a field added to a record, a case added to an
    `enum` — are called out, because the component model types both
    structurally.
  - **16b-2 — the version-bump check** ([#89](https://github.com/PromptPasture/jan-klod/issues/89)):
    warn when a `wit/*.wit` changes without the package version moving. A
    warning until the freeze, a failure after it.
  - **16b-3 — N-1 minor compatibility** ([#90](https://github.com/PromptPasture/jan-klod/issues/90)):
    deferred until the freeze. Below `1.0` a differing minor is refused by
    policy, so there is no version pair an adapter would help.

  **The freeze point.** Until the **first public release**, `wit/` may still
  change freely and the rules above describe intent. From that release they
  bind: a change to `wit/` requires the matching version bump, 16b-2's warning
  becomes a failure, and an incompatible `api-version` is a compatibility
  break rather than a development inconvenience. Nothing is tagged or released
  yet, so that point is still ahead — which is exactly why the rules had to be
  written before it rather than after.
- **16c — `ext install` with provenance.** a gateway `ext install <url|path>` subcommand:
  download to staging, checksum, signature (lean: minisign — small, no PKI),
  WIT validation, then move into `ext/`. A tampered artefact never reaches `ext/`.
- **16d — Registry index + `ext search`.** A static JSON index over HTTP (the
  Configurator's assumption) listing name, version, `api-version`, requested
  capabilities, checksum, signature; `ext search`/`ext list` read it; the
  Configurator shows requested capabilities before download.

**Exit gate:** a component whose manifest omits a capability it imports is refused
at boot; a tampered download is refused; an install from a static index fixture
works offline; a WIT major mismatch is a clear error.

## Phase 17 — Web client + GUI shell

**Goal:** one front-end codebase serves both the browser and the desktop window.

**Needs Phase 13c, and now owns the decision it was deferred for.** 13c stopped
on a question only this client can answer: the WebSocket needs a socket that can
time out a read and be read while written, `tiny_http` gives neither, and the
ways out (a second listener, or a listener the core owns and serves both from)
trade a bind address, an `Origin` check and a second place the token rule lives
against a rewrite of the REST surface. 17a should pick one and unblock
[#43](https://github.com/PromptPasture/jan-klod/issues/43); the measurements it
needs are already on that issue.

- **17a — Web client.** A static, dependency-light TypeScript SPA served by the
  core at `/`, speaking the protocol over WebSocket: sessions, streaming, `ask`,
  cancel — TUI parity. First first-party TypeScript in the repo: supply-chain
  hygiene (lockfile, `cargo-deny`-equivalent audit, no build framework beyond a
  bundler) is part of the slice.
- **17b — Tauri shell.** `jan-klod-ui --gui` opens a Tauri window over the same
  front-end; the `gui` bundle ships it. System webview, no bundled browser.

**Exit gate:** a browser and a Tauri window drive a turn with `ask` + cancel from
one front-end codebase.

## Phase 18 — Ecosystem ports

**Goal:** MCP and ACP in both directions. The inbound halves exist (`registry-mcp`,
`agent-*`); this phase adds the core *as* a server on each. Needs Phase 13a.

- **18a — Core as an MCP server.** a gateway `mcp` subcommand exposes the core over
  MCP stdio: an `ask` tool and session tools, so other agents can call jan-klod.
- **18b — ACP server side.** a gateway `acp` subcommand maps ACP onto the protocol so
  editors (Zed and others) connect without a bespoke plugin.
- **18c — `registry-mcp` stdio transport.** MCP servers over a long-lived child
  process — the Phase 7 "long-lived children" carry-forward — behind the Phase 15
  policy.

**Exit gate:** an ACP client fixture runs a turn against the core; an MCP client
lists and calls a core-exposed tool — both offline.

## Cross-cutting (continuous, not a phase)

Resource-budget and developer-experience items from the vision, tracked as
issues, not phases: **lazy guest instantiation** (instantiate a component on
first call, not at boot), an **AOT component cache** (`.cwasm` keyed by
component hash + Wasmtime version), an **accelerated `host-fs.grep`** (native
tree search behind the existing jail, the guest only shapes the request), the
**Rust extension PDK** (`make ext-new NAME=` template + guest test harness
guide), and **named distributions** in the Configurator (`coding`,
`headless-chat`, `minimal`).



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

Beyond the host-side store (durable KV/history persistence, delivered in Phase 3), a
coding agent benefits from *curated* memory — working vs episodic recall, semantic
search, consolidation. It is **unresolved whether jan-klod should ship this at all**:
it may belong in a third-party `store-*`/`tool-*` extension, an MCP server via
`registry-mcp`, or an external service, rather than a first-party contract. Decide
*if* before *how*; do not add a memory-curation interface speculatively (YAGNI).
Persistence is done; curation is deliberately parked as a question, and the
memory-tool row is excluded from the Phase 8 fleet accordingly.
