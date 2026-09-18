---
type: concept
title: Roadmap
description: Phased plan from foundation (Rust + Wasmtime + Component Model) to v0.1.0 runtime (Phases 1–12), then Harness-as-a-Platform (Phases 13–18): client protocol, event log, OS sandbox, signed registry, web/GUI, MCP/ACP ports; then the Enterprise Box (Phases 26–31): trusted delivery, secrets, identity, audit, batteries, distribution.
tags: [roadmap, planning, rust, wasmtime, component-model, phases, vision]
created: 2026-06-29
updated: 2026-09-18
status: v0.1.0 complete (Phases 1–12 done, nothing tagged or released yet); Harness as a Platform under way — Phases 13, 14, 15, 16, 17 and 18 done (13c **superseded by Phase 20**, 15d deferred to a Windows environment, 18c standing alone), 19 done (19a–19h), 20, 24 and 25 done, 21 in progress, 22 and 23 open; the Enterprise Box is phased as 26–31, none started
---

# Roadmap

This build sequence follows the foundation decision
[decisions/2026-06-29-component-model-rust](../decisions/2026-06-29-component-model-rust/Handoff.md).
It is a **validated-pivot-then-port**: the Go + Wazero MVP proved *architecture* end-to-end;
we now prove the *foundation* (Component Model on Rust + Wasmtime) by front-loading the risky
check as the first slice (a thin walking skeleton behind a go/no-go gate), then build out.

## Ground rules carried into every phase

- **`core` is Rust, and Rust only.** It is the small, rarely-changing container —
  config loader, extension registry, lifecycle, the Wasmtime component host, event
  bus, observability. Zero agent behaviour. See [Architecture](architecture.md).
- **Extensions are polyglot by design.** Each extension uses whatever language fits —
  any `wit-bindgen` language targeting the same [WIT contracts](contracts.md), interchangeable.
  Rust is one option, not required because `core` is Rust. (This is what the *ecosystem* can do.)
- **Our own built extensions default to Rust.** First-party extensions are Rust
  (shared types, single CI/lockfile/audit, no GC caveat). Non-Rust is used only when
  an ecosystem library makes it *decisive*, gated by CM-toolchain maturity and supply-chain
  posture (TS/JS and Python case-by-case; Kotlin/JVM excluded). Go (TinyGo) stays supported
  but not default. The polyglot boundary is proven by the Slice 1a TinyGo gate (`make gate`);
  no production Go extension is needed. The build pipeline is unprotected by the sandbox, so
  npm-style risk weighs on any non-Rust choice. See
  [decisions/2026-06-29-extension-technologies](../decisions/2026-06-29-extension-technologies/BRAINSTORM.md).
- **The launcher/updater is a tiny Go binary**, separate from `core` so it survives
  a core swap. See [Blue/Green Deployment](blue-green-deployment.md).
- **The WIT contracts already exist** (`wit/*.wit`, 15 interfaces) and are canonical.
  They survived the Go-source reset and are the fixed point everything builds against.
- **All implementation code lives under `src/`**, the Rust host workspace: five members
  (`core/`, `config/`, `host/`, `protocol/`, `tui/`) are direct children; `extensions/`
  and `gui/` are separate cargo workspaces; `supervisor/` (Go) and `web/` (TypeScript)
  have no cargo manifest; `wit/` stays at repo root. Detailed Phase 1 checklist:
  [PLAN.md](../decisions/2026-06-29-extension-technologies/PLAN.md).
- **Just-in-time library choices.** Every **(TBD)** in [Architecture](architecture.md)
  is resolved at the phase that first needs it (YAGNI), never speculatively.

## Starting point (after the reset)

The Go MVP source has been removed; its findings live in decision records
([2026-06-28-mvp-wasm-host](../decisions/2026-06-28-mvp-wasm-host/Handoff.md),
[2026-06-28-go-wasm-stack](../decisions/2026-06-28-go-wasm-stack/Handoff.md)). What carries forward:
the **`wit/` contracts**, architecture/concept docs, and validated behaviours (config-driven load,
lifecycle, host-http, OpenAI-compatible provider, in-memory store) — re-implemented as
real Component-Model code, not hand-rolled JSON ABI.

---

## Status tracker

Single source of truth. **The loop:** first phase not `done` → run slices → pass
**exit gate** → mark `done` → repeat. Update flags as state changes.

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Phase | Flag | Gate / note |
|---|---|---|
| 1 — Walking skeleton + foundation gate | `done` | **Slice 1a PASSED** (2026-06-29); [verdict](../decisions/2026-06-29-extension-technologies/SLICE-1A-GATE.md). **Slice 1b done:** `jan-klod-core` boots from `config.yaml` (registry, boot order, lifecycle, component host); host caps (`host-log`/`host-config`/`host-http`) as CM imports; three Rust guests build offline (`store-memory`, `provider-openai`, `manager-agent-loop` retiring in Phase 2); exit gate runs one turn in agent-loop guest (`tests/routing.rs`); supply-chain gates (`cargo-audit`/`cargo-deny`/`govulncheck` + SBOM) in [`.github/workflows/ci.yml`](../../.github/workflows/ci.yml) |
| 2 — Agent loop | `done` | **Re-architected 2026-07-01** ([decision](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md)): thin loop in **core** (`conductor` + `intercept` dispatcher); every decision is a sandboxed `interceptor-*` guest. Full v1 built (`intent-router`, `task-router`, `context`, `tool-selector`, `permission`). **Exit gate passed 2026-07-02:** intent → shape → ReAct → provider fallback → answer with permission-`ask`, offline. Carried-forward: streaming run-handle, `interceptor-llm` routing, `tool-callable` fleet. Checklist: [PLAN.md](../decisions/2026-07-01-phase2-agent-loop/PLAN.md) |
| 3 — Persistence + inbound network | `done` | [PLAN.md](../decisions/2026-07-02-phase3-persistence-network/PLAN.md) (2026-07-02). **Exit gate passed:** state survives `Runtime` restart via host-side SQLite (persistence host-side, not in-wasm; transcripts persisted) **and** HTTP client drives loop over REST (`jan_klod_core::serve` on `tiny_http`, via `jan-klod serve`). Boundary: store proxied host-side (no guest); REST host-side/sync (not `axum`). Carried-forward: SSE streaming, `host-storage` backed by `Store`. |
| 4 — Clients & integrations | `done` | [PLAN.md](../decisions/2026-07-02-phase4-clients-integrations/PLAN.md) (2026-07-02). **Exit gate passed:** UI client (`jan-klod-ui` — REPL + `ratatui` TUI over REST) drives core; `chat-telegram` message drives turn + reply, offline. `agent-*` ACP via `ToolInvoker::delegate`. Decisions: clients as separate processes over REST; Telegram needs no `host-socket`. Carried-forward: GUI (Tauri), ACP-over-HTTP, `host-socket`. **Extended 2026-08-09:** both surfaces now *ask* — REST/SSE driver serves socket mid-turn; Telegram asks in chat, takes next message as answer. |
| 5 — Distribution & ops | `done` | [PLAN.md](../decisions/2026-07-02-phase5-distribution-ops/PLAN.md) (2026-07-02). **Exit gate passed:** Go supervisor (`src/supervisor/`) runs blue/green flip→health→rollback (probes `/health`), unit-tested both ways; `make bundle` produces archive; extracted `jan-klod` boots offline. Deploy unit = core + guests. Carried-forward: staging, bundle matrix, web Configurator. |
| 6 — Streaming & steering | `done` | [PLAN.md](../decisions/2026-07-02-phase6-streaming-steering/PLAN.md) (2026-07-02). **Exit gate passed:** conductor emits events via `EventSink` → **SSE** over REST (`tiny_http`, consumed live by UI) → **cancel** (sink `Stop` + client-disconnect) → **steering** (`Driver::follow_up` injects cycle at `prepare-next-turn`). Carried-forward: per-token deltas, TUI/Telegram streaming. |
| 7 — File-workspace substrate | `done` | [PLAN.md](../decisions/2026-07-02-phase7-file-workspace-substrate/PLAN.md) (2026-07-02). **Exit gate passed:** **`host-fs`** (path-jailed read/write; `..`/absolute denied) and **`host-process`** (bounded exec — workspace-jailed, timeout, output cap), both unit-tested, driven across CM via probe guests through `tool_host::ToolExtension`. Default-deny + workspace-jailed. Carried-forward: symlink hardening, long-lived children, wiring into loop tools. |
| 8 — Tool fleet | `done` | [PLAN.md](../decisions/2026-07-02-phase8-tool-fleet/PLAN.md) (2026-07-02). **Exit gate passed:** model tool call runs full loop — advertised at `select-tools`, gated at `tool-call`, dispatched by `ToolFleet` to real tool writing via `host-fs`, result fed back, grounded. Fleet: `tool-fs` (read/write/grep), `tool-shell` (host-process); config instantiates enabled `tool.*` with default-deny substrates. **Extended 2026-08-08:** `tool-edit` (hash-anchored edits, [reliability lever](small-model-harness.md#edit-reliability-tool-edit--built); stale anchor rejected). `tool.fs`/`tool.edit` + permission **enabled in `config.yaml`** so fresh install reads/edits without hand-editing. `tool-find` (bounded glob via `host-fs`; visit/depth/byte caps). `tool-fs::grep` searches tree (optional `glob`, `path:lineno:line`). `guest-fs` library (workspace member, not component). `tool-git` (read-only: status/diff/log/show/branch) demonstrates shaping — guest argv from closed ops, write half *not expressible*. |
| 9 — Anthropic provider | `done` | **Done 2026-07-03.** `provider-anthropic` guest (Rust, `wasm32-wasip2`): native Messages API (`/v1/messages`), system-message extraction, `tool_use` → `ToolCallRequest`, stop-reason normalization, error mapping. Config: `extensions.provider.anthropic.enabled: true`. |
| 10 — Skills + MCP registry | `done` | **Done 2026-07-03.** `registry-skills` (scans `.agents/skills/*.md`, YAML `name:`/`description:`, exposes via `skill-registry` WIT); `registry-mcp` (SSE/HTTP MCP gateway, JSON-RPC `tools/list` + `tools/call`). `registry_host.rs` binds both; `CombinedFleet` dispatches to `ToolFleet` then `RegistryFleet`. `host-fs` added to `skill-registry-world`. |
| 11 — UX polish | `done` | **Done 2026-07-03.** REST migrated to resource model (`POST /turn` retired → `GET /sessions`, `POST /sessions`, `GET /session/:id`, `POST /session/:id/message`); `list_sessions`, `list_namespaces`; workspace auto-detect (default `$PWD`); per-token streaming in TUI. |
| 12 — Release: GitHub + web | `done` | **Done 2026-07-03.** GitHub Actions release (`.github/workflows/release.yml`; tag `v*` → matrix bundles + SHA256SUMS); `scripts/install.sh` (OS/arch detect, verify, install to `~/.local/bin`); GitHub Pages landing, quickstart, README. |
| 13 — Client protocol | `done` | [#35](https://github.com/PromptPasture/jan-klod/issues/35). [Vision](../decisions/2026-09-08-harness-platform-vision/Vision.md) decision 1. **13a done 2026-09-09** ([#41](https://github.com/PromptPasture/jan-klod/issues/41)): `jan-klod-protocol` — 8 commands, 8 notifications, `PROTOCOL_VERSION`, JSON Schema + drift test, SSE lossless proof. **13b done 2026-09-10** ([#42](https://github.com/PromptPasture/jan-klod/issues/42)): `jan-klod-gateway rpc` on stdin/stdout; `jan-klod` spawns by default (no port/token). Framing in contract (core writes, clients read); `turn/follow-up` over stdio only. Gate met: turn streams, `ask` answered on pipe, `turn/cancel` stops (proven by never asking), REST + SSE pass. **13c (WebSocket) superseded by Phase 20** (#169), deferred until Phase 17 needed it—which didn't (#43). `tiny_http` cannot timeout-and-write-simultaneously; socket decision deferred. |
| 14 — Event-sourced session log | `done` | [#36](https://github.com/PromptPasture/jan-klod/issues/36). Vision decision 3. **Exit gate passed 2026-09-09:** append-only `events` table with versioned envelope (#44); transcript/resume/fork as projections (#45). Resumed session rebuilds from log; fork at seq *N* runs independently. Only events write transcripts; pre-log DB converted at boot. Gate: transcript rebuilt post-restart equals pre-restart; fork independent. |
| 15 — OS-level effect sandbox | `done` | [#37](https://github.com/PromptPasture/jan-klod/issues/37). Vision decision 2. **15a done 2026-09-09** ([#46](https://github.com/PromptPasture/jan-klod/issues/46)): `execution.sandbox` policy, `SandboxBackend` seam, boot-time resolution never downgrades, `require: true` denies exec. **15b done 2026-09-10** ([#47](https://github.com/PromptPasture/jan-klod/issues/47)): macOS runs under Seatbelt (kernel-enforced writable boundary); `require: true` permits. Linux approval-only (waits 15c). Per-turn warning deferred to 16a. **15c done 2026-09-10** ([#48](https://github.com/PromptPasture/jan-klod/issues/48)): Linux runs under Landlock (gateway applies before exec, no `pre_exec`/`unsafe`, ABI 4 denies network). Gate met on both. **15d deferred** ([#49](https://github.com/PromptPasture/jan-klod/issues/49)): Windows approval-only until environment exists. Asymmetry closed **2026-09-12** ([#95](https://github.com/PromptPasture/jan-klod/issues/95)): `ci-macos.yml` runs Seatbelt on `macos-latest` (both backends CI-verified). Schedules differ: Landlock every push, Seatbelt on change (macOS billing 10×). Gate: write outside workspace denied on macOS/Linux; else **approval-only** at boot. Per-turn warning deferred because nothing yet distinguishes process-using tools from read-only tools. Phase closed when exit criteria met (#47 needed macOS runner #95 landing 2026-09-12). |
| 16 — Capability manifest + signed registry | `done` | [#38](https://github.com/PromptPasture/jan-klod/issues/38). Vision decision 4. **16a done** (#86, #87): guest ships manifest from imports; host refuses missing/under-declared/incompatible manifests (cross-validated). **16b-1/-2 done** (#88, #89): versioning rules written; `make wit` warns on version mismatch (baseline by tag/merge-base/HEAD~1), entered CI. 16b-3 (N-1 compat) deferred (#90). **16c-1/-2 done** (#91, #92): `ext install` (path or URL) verifies before landing: digest, minisign over component+manifest via `registry.trusted-keys`, validity, consistency. Signed default; `--allow-unsigned` requires `--sha256`. Key list empty; installs need `--allow-unsigned --sha256` until 16c-3 (#93). Staging in `ext/.staging/`. **16d done** (#137–140): `make registry-index` writes deterministic `index.json` (not tarball) of `.wasm` + `.minisig`. Sorted, unique, checksum validated on every run. `ext search`/`ext list --remote` read index from `registry.url`, print capabilities before download. `ext install <name>` resolves via index, refuses tampering (digest + signature). `http`/`https` fetched under egress policy; else path. GitHub Pages publish unverified until tag pushed. **Exit gate 2026-09-12:** manifest-omitted capability refused at boot (16a); tampered download refused (16c); offline fixture install works (16d-2); WIT version mismatch is clear error (16b). |
| 17 — Web client + GUI shell | `done` | [#39](https://github.com/PromptPasture/jan-klod/issues/39). Vision decision 5. **17a done** ([#54](https://github.com/PromptPasture/jan-klod/issues/54)): TypeScript SPA served at `/` over REST + SSE (13c not prerequisite; #43 deferred pending browser evidence). **17b done** (#141–144): `jan-klod --gui` opens Tauri 2 over same front-end, from `src/gui` (separate workspace; Tauri costs +256 packages, 332 CPU-s, 838 MB, 11 deny entries—escape hatch measured, does not exist; MPL floor in `wry`). `make bundle GUI=1` second axis. Gate: browser **and** window drive turn with `ask`+cancel. Browser met (`web_client.rs`); window partly met (SPA boots in webview, real gateway, seeded token auth, identical assets; no typing/ask/cancel, Linux unexercised—#142 needs human). Closed when exit criteria met. |
| 18 — Ecosystem ports | `done` | [#40](https://github.com/PromptPasture/jan-klod/issues/40). Vision decision 6. Needs 13. **18a/18b done** (#56, #57): `jan-klod-gateway mcp` (ask/session_list/session_get over MCP stdio), `jan-klod-gateway acp` (ACP agent side)—both method adapters over `rpc`, no SDK/dep. MCP client lists/calls tool; ACP fixture runs turn, offline. First surface where core originates JSON-RPC + serves it (editor can answer; MCP cannot). Note: MCP turn cannot write/run (no responder, default refusal). **Closed** (umbrella closes at gate meet): tested (`cargo nextest run -p jan-klod-host mcp:: acp::`, 13 passed). **18c** (#58→#109/#110, done) gives `registry-mcp` stdio over long-lived child (inbound, core as MCP client; gate only named server, never gated phase). Gate: ACP client runs turn; MCP client lists/calls—offline. |
| 19 — Terminal client experience | `done` | [#97](https://github.com/PromptPasture/jan-klod/issues/97). Not a vision decision—client experience. **19a done** ([#98](https://github.com/PromptPasture/jan-klod/issues/98)): `src/core/ui/src/theme.rs` owns colour/glyphs/capability; test greps so no `Color` literal escapes. **19b–19h done** (#99–105): chat, tool blocks, composer, lifecycle, permission modal, frame, switcher/help/quit. Key findings: `Enter` mid-turn needs three signals (cost: message into live turn); ask dialog answered with wrong session (bug predicted/found); session id `&str` became owned when switching sessions mid-run. Gate: 120×32 + 60×20 terminals render correctly; `NO_COLOR=1` 16-colour loses no info. |
| 20 — axum inbound surface | `done` | [#169](https://github.com/PromptPasture/jan-klod/issues/169), slices [#223](https://github.com/PromptPasture/jan-klod/issues/223), [#225](https://github.com/PromptPasture/jan-klod/issues/225), [#224](https://github.com/PromptPasture/jan-klod/issues/224), [#226](https://github.com/PromptPasture/jan-klod/issues/226), [#229](https://github.com/PromptPasture/jan-klod/issues/229)–[#231](https://github.com/PromptPasture/jan-klod/issues/231). [Decision](../decisions/2026-09-13-rest-concurrency-and-the-send-session/Handoff.md), and [two turns at once](../decisions/2026-09-18-two-turns-at-once/Decision.md). **Exit gate passed:** *two clients drive turns in two sessions concurrently over REST, with an `ask` answered in one while the other streams* — `host/tests/it/gate.rs::phase20_exit_gate_two_sessions_at_once`, whose control in the same file is the identical pair in **one** session, which serialises. The order the slices ran in is not the order they were filed in, and twice for the same reason: `PromptDriver::wait_for_answer` **was** an accept loop, so the confirmation path had to be dismantled before the transport could change (#225 ahead of #224), and nothing in the filed list reached the gate at all until [#228](https://github.com/PromptPasture/jan-klod/issues/228) was filed mid-phase. Measured: `axum` **net +13 packages** (16 added, 3 removed with `tiny_http`), `ws` **+8** including a second major of `rand`, both green against an **unmodified** `deny.toml`; an extra `AgentSession` **1.35 MB and 3.5 ms**, which is what made an agent per session the cheaper route than a resumable turn. Carried forward: 20f (`AgentSession: Send`) still unfiled and still unneeded; [#232](https://github.com/PromptPasture/jan-klod/issues/232) — per-session state changes what `always allow` means. |
| 21 — Seven `jk-*` crates | `in-progress` | [#172](https://github.com/PromptPasture/jan-klod/issues/172). `jan-klod-core` was **21,170 lines** holding kernel, session store and every surface at once. Transport already lives in `jan-klod-host` (`rpc`/`serve`/`acp`/`mcp`/`telegram`/`ws`), so [Architecture `:29`](architecture.md) is accurate again rather than false. First slice landed ([#180](https://github.com/PromptPasture/jan-klod/issues/180)): the session store moved out as `jk-session` (`event_log`/`projection`/`store`, no `wasmtime` in its dep tree); the turn vocabulary (`Event`/`Flow`/`EventSink`/`Role`/`Message`/`ToolCall`/`ToolOutcome`/`UserPrompt`/`Driver`) moved down into `jan-klod-protocol::turn`, the shared floor `jk-session` and the loop both sit above; `host-storage` now reaches the store through an `Entries` trait, not a direct `jk-session` dependency, keeping `jk-wasm` and `jk-session` unranked against each other. `jan-klod-core` is down to **16,591 lines**. Remaining split: `jk-protocol`/`jk-config`/`jk-wasm`/`jk-agent`/`jk-host`/`jk-tui`; package names become `jk-*`, **binaries keep `jan-klod`/`jan-klod-gateway`**. Slices [#179](https://github.com/PromptPasture/jan-klod/issues/179)–[#183](https://github.com/PromptPasture/jan-klod/issues/183). Gate: no upward dependency, `jk-tui` pulls no `wasmtime`, `jk-agent` holds no surface. |
| 22 — `host-agent` capability | `open` | [#173](https://github.com/PromptPasture/jan-klod/issues/173). A guest cannot drive a session today, which is why Telegram is kernel code and session export cannot exist. New WIT mirroring `jan-klod-protocol` inward, default-deny + manifest-declared. Slices [#184](https://github.com/PromptPasture/jan-klod/issues/184)–[#186](https://github.com/PromptPasture/jan-klod/issues/186). Gate: a chat turn with an `ask`, and no Telegram code in any Rust crate. |
| 23 — Polyglot, proven | `open` | [#174](https://github.com/PromptPasture/jan-klod/issues/174). [Architecture `:54`](architecture.md) promises any `wit-bindgen` language; the evidence is one TinyGo spike and five Rust `ext-new` templates. TS ([#187](https://github.com/PromptPasture/jan-klod/issues/187)) and Python ([#188](https://github.com/PromptPasture/jan-klod/issues/188)) guests + `ext-new LANG=`. Gate: both built by `make gate` and called in a turn, no host special case. |
| 24 — Client-surface contract | `done` | [#175](https://github.com/PromptPasture/jan-klod/issues/175), slices [#189](https://github.com/PromptPasture/jan-klod/issues/189)–[#191](https://github.com/PromptPasture/jan-klod/issues/191). **Exit gate passed:** `interceptor-system` contributes a `prompt` command and a `prompt-source` status item; the TUI and the browser both render and invoke them with no knowledge of that extension, and the window inherits the browser's. `wit/client-surface.wit` holds the two rules both renderers follow — contributed text is data, and a client that renders none of it runs turns unchanged. The host **probes** each component for the export rather than any world requiring it, so nineteen existing guests were untouched. |
| 25 — Self-extension | `done` | [#176](https://github.com/PromptPasture/jan-klod/issues/176), slices [#192](https://github.com/PromptPasture/jan-klod/issues/192), [#213](https://github.com/PromptPasture/jan-klod/issues/213), [#214](https://github.com/PromptPasture/jan-klod/issues/214), [#218](https://github.com/PromptPasture/jan-klod/issues/218), [#221](https://github.com/PromptPasture/jan-klod/issues/221). **Exit gate passed:** one session compiles a component in the jail, writes its manifest, hashes it, installs it unsigned with that digest, adopts it between turns and calls it — no gateway restart, no operator command (`host/tests/it/self_extend_chain.rs`). The posture is the one shipped: `writable: ["."]`, `network: false`, `require: true`, and the install grant is the local `path` form only — `registry.trusted-keys` stays empty, so the registry half still refuses every name. Two things the plan did not predict. **The agent must author the component's manifest**, because the installer refuses one nobody can inspect and cross-checks the declared capabilities against the artefact's real imports — a harder step than the digest, which is one `shasum` away. And **offline dependency resolution needs preparing**: a lock plus the shared cargo cache (~1 MB) rather than the 34 MB vendor tree [#192](https://github.com/PromptPasture/jan-klod/issues/192) measured, since the jail allows reads wholesale and `CARGO_HOME` is already in `BASE_ENV`. Left open: [#222](https://github.com/PromptPasture/jan-klod/issues/222), a stem naming no category adopts into nothing and reports success. |
| 26 — Trusted delivery | `not-started` | [Vision — The Enterprise Box](../decisions/2026-09-18-enterprise-box/Vision.md), decision 3. Phase 16 built signing and a registry index and shipped both inert: `registry.trusted-keys` is empty, no default `registry.url` exists, and `git tag` returns nothing, so `ext install` refuses by construction and `install.sh` has nothing to fetch ([#93](https://github.com/PromptPasture/jan-klod/issues/93)). Slices: keypair and CI secret, SBOM plus signatures on the release, `v0.1.0` tagged, shipped config trusting the published key, offline install proven. Blocked on a human — the key has to be generated before any of it runs. Gate: an operator on a network reaching nothing but a self-hosted model verifies the archive against the published key, installs an extension by name from the bundled index, runs a turn; a tampered archive is refused with the reason named. |
| 27 — Secrets, and one place redaction happens | `not-started` | [Vision](../decisions/2026-09-18-enterprise-box/Vision.md), decision 5. Keys are `${VAR}` expansion into a config string (`src/config/src/lib.rs`) and nothing keeps one out of the event log, an SSE frame or a rebuilt transcript. New `wit/host-secrets.wit` (`get(name) -> result<string, error>`, manifest-gated like every other capability), backends for macOS Keychain and Linux libsecret with the environment as fallback, and redaction at `PersistingSink` — the one point every persisted and streamed string passes — with patterns read from the `interceptor-guardrails` rule set that is already data. Independent of 26 and of 21. Gate: a provider reads its key through `host-secrets`, the model echoes it verbatim, and neither log nor live client nor restarted transcript contains it, while an extension that did not declare the capability cannot read it at all. |
| 28 — Identity at the boundary | `not-started` | [Vision](../decisions/2026-09-18-enterprise-box/Vision.md), decision 4. `authorised()` in `src/host/src/serve.rs` compares one shared secret; it establishes no principal, and every caller sees every session. The credential resolves to a principal at the guard, `jk-session` scopes sessions to it, the principal reaches `user-turn` in `wit/interceptor.wit` as an optional field, and `interceptor-rbac` is a reference guest with roles as data, off by default. **Phase 21 is not a prerequisite** — transport already lives in `jan-klod-host`; 21e ([#183](https://github.com/PromptPasture/jan-klod/issues/183)) changes the package name, which is a merge-order concern. Identity asserted by a client or by a guest is declined. Gate: two principals each run a turn, each sees only their own sessions, and the RBAC guest stops one at `before-loop` rather than at the tool, naming the principal in the record. |
| 29 — An audit trail that survives a review | `not-started` | [Vision](../decisions/2026-09-18-enterprise-box/Vision.md), decisions 6 and 8. The log records everything and nothing reads it out ([#186](https://github.com/PromptPasture/jan-klod/issues/186)). Each event row carries the previous row's digest; session export is an extension over `host-agent` emitting JSONL and respecting 27d's redaction; `architecture.md:27` stops promising Prometheus and OpenTelemetry in the same change that adds a `tracing` subscriber emitting structured JSON. The export slice waits on **Phase 22**; the chain and the logging slices do not. The hash chain detects edits by anything that is not the writer — the host owns the database, and the security-model row has to say so. Gate: a ten-turn session exports to JSONL and re-imports to the identical transcript, the chain verifies, one altered row breaks it, and no document claims observability the binary lacks. |
| 30 — The batteries | `not-started` | [Vision](../decisions/2026-09-18-enterprise-box/Vision.md), decision 7. Measured against [`pi-onboard`](https://github.com/PromptPasture/pi-onboard)'s capability groups, three are absent: persistent memory, scheduled tasks and sub-agents. `src/core/src/delegate.rs` is written, unit-tested and never instantiated, because `CATEGORIES` at [`src/core/src/lib.rs:259`](../../src/core/src/lib.rs) has no `agent`. Order is cheapest-useful-first: `tool-memory` over `host-storage` (no new contract), `tool-web-search`, `interceptor-persona`, then sub-agents and scheduling (both wait on **Phase 22**), then Bedrock and Vertex as their own guests — SigV4 and Google OAuth are not a base-URL change, while self-hosted OpenAI-compatible endpoints already work through `provider-openai` and need documentation, not code. **This closes the curated-memory open question below:** episodic recall ships, curation does not. Gate: one session stores a fact, recalls it a turn later, searches the web, delegates to a sub-agent and answers under a named persona, with nothing installed by hand. |
| 31 — The enterprise distribution and the guided first run | `not-started` | [Vision](../decisions/2026-09-18-enterprise-box/Vision.md), decision 2. Four distributions exist and none is enterprise-shaped, and they disagree on the count: `scripts/install.sh:35` knows four, the landing page's table (`pages/index.md:44`) lists three and omits `self-extend` — the drift [#178](https://github.com/PromptPasture/jan-klod/issues/178) tracks. `scripts/distributions/enterprise/` is one more guest-list-and-config pair held to the existing agreement test, and `jan-klod-gateway setup` audits before it writes, offers capability groups, previews, then applies atomically — the shape [`pi-onboard`](https://github.com/PromptPasture/pi-onboard) proved, with its three configuration strategies. Consumes 26–30. Gate: a fresh machine reaches a running, signed, audited install without opening `config.yaml` once, and a second `setup` with the same answers writes nothing. |

Outside the phases: [#177](https://github.com/PromptPasture/jan-klod/issues/177) (`interceptor-guardrails` — content policy) **shipped**, in three slices and with no contract change, as predicted. The guest reviews four phases (`tool-call`, `after-response`, `finalize`, `select-context`) against rules that are **data** — pattern sets with a decision each, read through `host-config`, matched by the `regex` crate in linear time because matching sits on the path of every tool call. Off by default and off even when enabled until rules exist; on in the `coding` distribution for redaction, and in `self-extend` for `deny-tool-arguments` too, since that is the distribution where a shell is reachable. A malformed rule set invalidates the whole set and the host fails closed at `tool-call` — a typo stops tool calls rather than leaving a guardrail silently absent. `docs/concepts/security-model.md` carries the row and names the two tests.
[#178](https://github.com/PromptPasture/jan-klod/issues/178) (retire stale doc claims and superseded records) is `blocked` on two calls: whether to write decision records for the ten record-less completed phases, and whether `make lint-docs` should exist.

Built-extension language assignments and their own status live in the
[Extension Technologies brainstorm](../decisions/2026-06-29-extension-technologies/BRAINSTORM.md#near-term-assignments-provisional--confirmed-at-the-phase-1-gate).

---

A completed phase that has a decision record keeps its goal and a link; the
record holds the plan and the tracker row above holds what shipped. That is not
only pruning — several of those sections describe a design the build then
changed, and a reader met the plan before the outcome. Phase 3's `host-serve`
capability and `api-rest` guest are the clearest case: what exists is a
host-side REST surface, which the tracker row says and the section did not.

Completed phases with **no** record keep their detail in full. For those the
section is the only account there is, and pruning it would delete rather than
compress.

## Phase 1 — Walking skeleton + foundation gate

**Goal:** prove the foundation on *real* code (no throwaway spike) and then reach the Go MVP's parity on it. The risky foundation check is front-loaded as the first slice.

Planned in [Slice 1a gate](../decisions/2026-06-29-extension-technologies/SLICE-1A-GATE.md) · [PLAN](../decisions/2026-06-29-extension-technologies/PLAN.md). What shipped is the tracker row above.

## Phase 2 — First real value: the agent loop

**Goal:** the runtime does something useful end-to-end. **Re-architected 2026-07-01** ([Thin Loop + Interceptor Middleware](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md)).

Planned in [PLAN](../decisions/2026-07-01-phase2-agent-loop/PLAN.md). What shipped is the tracker row above.

## Phase 3 — Persistence + inbound network

**Goal:** durable state and outside reach to core.

Planned in [PLAN](../decisions/2026-07-02-phase3-persistence-network/PLAN.md). What shipped is the tracker row above.

## Phase 4 — Clients & integrations

**Goal:** human and agent surfaces.

Planned in [PLAN](../decisions/2026-07-02-phase4-clients-integrations/PLAN.md). What shipped is the tracker row above.

## Phase 5 — Distribution & ops

**Goal:** ship and keep updatable.

Planned in [PLAN](../decisions/2026-07-02-phase5-distribution-ops/PLAN.md). What shipped is the tracker row above.

## Phase 6 — Streaming & steering

**Goal:** loop streams incrementally; driver interrupts/steers (Phase 2 carry-forward).

Planned in [PLAN](../decisions/2026-07-02-phase6-streaming-steering/PLAN.md). What shipped is the tracker row above.

## Phase 7 — File-workspace substrate

**Goal:** mediated file and process access (core capabilities for all file/execution tools; sandbox grants none by design).

Planned in [PLAN](../decisions/2026-07-02-phase7-file-workspace-substrate/PLAN.md). What shipped is the tracker row above.

## Phase 8 — Tool fleet

**Goal:** the ordinary sandboxed `tool-*` components — cheap to add once Phase 7 lands — grouped by the capability they route through. The only shared design work is routed I/O: file/exec tools go through `host-fs`/`host-process`, never raw OS.

Planned in [PLAN](../decisions/2026-07-02-phase8-tool-fleet/PLAN.md). What shipped is the tracker row above.

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

**Goal:** workflow shortcuts and ecosystem tool access.

- **`registry-skills`:** scans `.agents/skills/` Markdown (YAML `name:`), exposes via `list-skills`/`invoke`/`reload`. `ToolFleet` calls at `select-tools`/dispatch.
- **`registry-mcp`:** connects MCP servers via SSE (stdio deferred—needs long-lived `host-process`). Exposes `list-tools`/`invoke-tool`/`reconnect`. Guest narrows to `publish`. Permission gate on MCP calls.
- **Prerequisite:** `host-event` granted to `mcp-registry-world`.

**Exit gate:** model calls MCP tool via SSE stub (offline); invokes skill, template injected.

## Phase 11 — UX polish

**Goal:** daily-use complete (can start in parallel with 9–10).

- **Per-token streaming in TUI:** `ratatui` consumes `text-delta` SSE incrementally. Wiring only.
- **Session list + resume:** `GET /sessions` returns past sessions (id/created/preview). `--session <id>` resumes; `POST /turn` replays history.
- **Workspace auto-detect:** `jan-klod serve` defaults to `$PWD` when `workspace:` absent.

**Exit gate:** launch in repo, message, disconnect, resume by id, stream per-token in TUI.

## Phase 12 — Release: GitHub + web

**Goal:** find, install, work in under 15 minutes.

- **GitHub releases:** Actions on `git tag v*` → matrix bundles (linux/darwin × x86_64/arm64) + SHA256SUMS.
- **Install script** `scripts/install.sh`: detect OS/arch, download, verify, install to `~/.local/bin`.
- **GitHub Pages:** static site (what/differentiator/install/quickstart).
- **Quickstart:** install → API key → serve → TUI → edit file. <500 words.
- **README:** what/differentiator/install/quickstart/screenshot.

**Exit gate:** cold-start install + quickstart file edit in under 15 minutes.

---

## Post-v0.1: Harness as a Platform (Phases 13–18)

[Vision — Harness as a Platform](../decisions/2026-09-08-harness-platform-vision/Vision.md)
(2026-09-08): jan-klod as **agent runtime** (kernel + distributions + clients). Phased by dependency:
protocol (13) gates web (17) + ecosystem ports (18); event log (14) independent; sandbox (15) + registry (16)
parallel with 13–14. Work: one umbrella issue per phase ([#35–40](https://github.com/PromptPasture/jan-klod/issues/35)),
one per slice; cross-cutting [#59–63](https://github.com/PromptPasture/jan-klod/issues/59). Library choices just-in-time.

## Phase 13 — Client protocol

**Goal:** client surface becomes WIT-rank contract (versioned schema, compatibility tests) so
TUI/web/GUI/editors/scripts share wire format. Supersedes Phase 3 "UI via REST"; REST + SSE as protocol *projection*.

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
  justify. **17a shipped without one** (2026-09-12): the existing REST + SSE
  surface already pushes an `ask`, takes its answer and cancels, so the browser
  client was built on that and 13c stayed deferred — it now waits on evidence
  that the SSE teardown is the wrong shape for a browser, not on a slice.
  Measurements worth keeping: `tungstenite`
  costs 6 packages without its `handshake` feature and ~10 with it; a hand-rolled
  RFC 6455 handshake costs `sha1` alone, 1 package.

**Exit gate:** `jan-klod-ui` drives a full turn — streaming, `ask`, cancel — over
stdio JSON-RPC; REST + SSE tests still pass; protocol version negotiated at connect.

## Phase 14 — Event-sourced session log

**Goal:** the session's canonical record is the event stream the clients already consume, not a transcript. Transcript, `GET /session/:id`, and SSE become projections; resume, fork, replay and audit all fall out.

- **14a — Event table + writer sink. Done 2026-09-09** ([#44](https://github.com/PromptPasture/jan-klod/issues/44)).
  The append-only `events` table (`session`, `seq`, `ts`, `kind`, `payload`, keyed on `(session, seq)`) in the host-side SQLite `Store`; `PersistingSink` fans out every conductor `Event` to the log without taking the stream from the SSE/TUI sinks, and `PersistingDriver` records the `ask`, its answer and any steering follow-up — the last of these required to rebuild steered turns. The user message is logged first, so a session's log opens with its own input. The permission decision needs no kind of its own: a denial is the `Warning` the conductor already emits, an approval is the `ask`/`answer` pair. `EVENT_LOG_VERSION` is independent of the protocol version. Wired in `run_and_persist`, the one funnel all eight turn entry points share. Text deltas are not coalesced. See [Architecture → Storage](architecture.md#storage).
- **14b — Projections + resume + fork. Done 2026-09-09** ([#45](https://github.com/PromptPasture/jan-klod/issues/45)).
  `core::projection::transcript` is a pure function over log rows; `replay`, `AgentSession::transcript`, `list_sessions`, `GET /session/:id` (now serving `messages`, not `turns`) and `GET /sessions` all read through it. `POST /session/:id/fork` copies a prefix into a new session that then diverges, with `session/fork` in the protocol crate as contract-only until a transport lands. The `entries` transcript write is **gone**, and a one-shot conversion at boot turns pre-log databases into events — required, not optional, since the read surfaces no longer look at `entries`.

**Exit gate:** after a restart, a resumed session's transcript rebuilt from the log equals the pre-restart transcript exactly; a fork from event *N* runs independently and stays correct after another restart, each with its own branch of the log.

## Phase 15 — OS-level effect sandbox

**Goal:** confine what a `host-process` command *does*, not only who may call it. Closes the Phase 7 "OS isolation" carry-forward and the [security-model gap](security-model.md#known-gaps) it left.

- **15a — Policy object + approval-only mode. Done 2026-09-09** ([#46](https://github.com/PromptPasture/jan-klod/issues/46)).
  `execution.sandbox` in `config.yaml`: `mode: os | approval-only`, `writable: [paths]`, `network: bool`, `require: bool` (denies `host-process` outright rather than degrading, for operators who prefer no command to an unconfined one). `core::sandbox` holds policy; `SandboxBackend` trait and `NoBackend`; boot resolves effective mode and prints reason whenever it is not the one requested. No backend on any platform yet, so every platform is approval-only (15b changed that for macOS). Per-turn warning deferred to 16a — 16a's component-import introspection answers which tools use `host-process`.
- **15b — macOS Seatbelt backend. Done 2026-09-10** ([#47](https://github.com/PromptPasture/jan-klod/issues/47)).
  A generated `sandbox-exec` profile — `(deny default)`, reads allowed, writes only under `writable`, network per policy — applied by rebuilding every command as `sandbox-exec -p <profile> -- <command>`. Three findings: (1) Seatbelt matches the *resolved* path; `/tmp` symlink to `/private/tmp` breaks uncanonicalized profiles; in-workspace-write test catches this. (2) `SandboxBackend` now takes and returns an owned command, fitting both wrapper (Seatbelt) and in-process (Landlock). (3) Paths with unescapable characters (e.g., `"` in SBPL) are refused, not escaped. On macOS `require: true` now permits commands. **Not verified by CI** — tests are macOS-only ([#95](https://github.com/PromptPasture/jan-klod/issues/95)).
- **15c — Linux Landlock backend. Done 2026-09-10** ([#48](https://github.com/PromptPasture/jan-klod/issues/48)).
  Landlock filesystem rules, network denial via Landlock's own net rules, and fallback to approval-only on kernels without Landlock. Three findings: (1) No seccomp — Landlock ABI 4 (kernel 6.7) can express network denial; below that, runtime refuses the command rather than adding another mechanism. (2) No `unsafe` either — gateway re-executes itself under a `confine` subcommand, restricts itself, and `exec`s the command; both backends now share that wrapper shape. (3) Verified on Linux in container before it was written. **CI runs these tests on every push** — `ubuntu-latest` has Landlock; both halves guarded now.
- **15d — Windows spike. Deferred 2026-09-10** ([#49](https://github.com/PromptPasture/jan-klod/issues/49)) until after the first release or until a Windows environment exists to run it in. Feasibility of a restricted token / AppContainer for a spawned command, recorded as a dated decision. **It does not gate this phase**; standard: spike's value is a *verified* answer to "can this deny a write and deny network, without admin", and there is no Windows machine here to verify it on. Approval-only stays the default: `host_backend()` returns `None`, effective mode resolves to `approval-only`, boot prints the reason naming the platform, `require: true` refuses execution outright. Windows is honest; 15d only decides whether it can be better.

**Exit gate:** a `tool-shell` command writing outside the workspace is denied on macOS and Linux; elsewhere the run reports approval-only; the security-model row for `host-process` cites the new tests. It holds **by construction**, not by observation. `sandbox::tests::a_platform_with_no_backend_says_which_platform` asserts it, but it is `#[cfg(not(any(target_os = "macos", target_os = "linux")))]` and neither CI nor the development machine this was written on ever compiles it, let alone runs it. Verifying it on a real Windows run is 15d's second Acceptance line, which is why the deferral above leaves this clause proved by reasoning rather than by a test result. The middle clause is the one to read carefully: the right guarantee is honest — Windows defaults to approval-only and states that plainly.

## Phase 16 — Capability manifest + signed registry

**Goal:** an extension declares what it needs before it is loaded, the host cross-checks the declaration against the component's real imports, and installs are verified. Extends the Phase 5 "staging" carry-forward; prerequisite for a public extension ecosystem.

- **16a — Manifest + boot cross-check.** Split in two ([#50](https://github.com/PromptPasture/jan-klod/issues/50)), because the refusal cannot land before the manifests exist without breaking the boot of every shipped guest at once.
  - **16a-1 — manifest format. Done 2026-09-09** ([#86](https://github.com/PromptPasture/jan-klod/issues/86)).
    `ext/<name>.manifest.toml` beside each `.wasm`: `name`, `version`, `api-version`, `kind`, `description`, and `capabilities` read from the component's own imports rather than written by its author, so a manifest cannot claim less than the artifact beside it. Generated by `make -C src/extensions manifests`, not committed — `ext/` is build output. Nothing reads one yet.
  - **16a-2 — boot cross-check. Done 2026-09-10** ([#87](https://github.com/PromptPasture/jan-klod/issues/87)).
    `Runtime::boot` reads each component's real imports through wasmtime's component-type API and refuses three things: a capability imported but not declared (naming the interface), a component with no manifest at all, and one built against an incompatible `jan-klod:interfaces` version (naming both). Top-level `allow-unmanifested: true` is the named widening. Over-declaring is deliberately allowed and grants nothing. Per-extension import set kept — fact [#46](https://github.com/PromptPasture/jan-klod/issues/46)'s deferred per-turn sandbox warning was waiting for.
- **16b — WIT versioning policy.** Split three ways ([#51](https://github.com/PromptPasture/jan-klod/issues/51)), because the refusal already landed with 16a and the adaptation has nothing to adapt yet.
  - **16b-1 — the rules, written down. Done 2026-09-10** ([#88](https://github.com/PromptPasture/jan-klod/issues/88)).
    [Contracts → Versioning](contracts.md#versioning) states what counts as a major, minor or patch change to `wit/`, where the version lives, and why **pre-1.0 is stricter** rather than laxer: a `0.x` version carries no compatibility promise, so a differing minor is refused. Two changes that look additive and are not — a field added to a record, a case added to an `enum` — are called out, because the component model types both structurally.
  - **16b-2 — the version-bump check. Done 2026-09-10** ([#89](https://github.com/PromptPasture/jan-klod/issues/89)).
    `scripts/wit-version-check.sh`, run by `make wit`: warns when a `wit/*.wit` changed without the package version moving. A warning until the freeze, a failure after it. `lint-test` now runs `make wit` and sets `fetch-depth: 2`.
  - **16b-3 — N-1 minor compatibility** ([#90](https://github.com/PromptPasture/jan-klod/issues/90)): deferred until the freeze. Below `1.0` a differing minor is refused by policy, so there is no version pair an adapter would help.

  **The freeze point.** Until the **first public release**, `wit/` may still change freely and the rules above describe intent. From that release they bind: a change to `wit/` requires the matching version bump, 16b-2's warning becomes a failure, and an incompatible `api-version` is a compatibility break. Nothing is tagged or released yet, so that point is still ahead — which is exactly why the rules had to be written before it rather than after.
- **16c — `ext install` with provenance.** Split three ways ([#52](https://github.com/PromptPasture/jan-klod/issues/52)), because the local path carries the whole verification and the network only adds where the bytes come from.
  - **16c-1 — install from a local path, verified before it lands. Done 2026-09-11** ([#91](https://github.com/PromptPasture/jan-klod/issues/91)).
    `ext install`, `ext list`, `ext remove`. The pair is staged in `ext/.staging/<name>/` and checked — digest if given, minisign signature over **both** files under a key from `registry.trusted-keys`, the component compiles, its manifest matches its real imports — then moved with an atomic rename. A refusal leaves `ext/` byte-identical. Four findings: (1) boot's manifest cross-check became one `core::inspect` function (reusing prevents silent, security-relevant divergence); (2) signature covers manifest too (protects against attacker's description of capabilities); (3) fixtures from `ring` + `blake2` (discover installer needs prehashed signatures); (4) `Config::top_level` stops before env-expanding credentials.
  - **16c-2 — install from a URL. Done 2026-09-11** ([#92](https://github.com/PromptPasture/jan-klod/issues/92)).
    URL names the component; the manifest and both `.minisig` files are fetched from beside it, the same layout as on disk, and everything goes through 16c-1's `install` unchanged. Three findings: (1) fixed egress-policy hole ([#107](https://github.com/PromptPasture/jan-klod/issues/107)) — ureq followed redirects unchecked; (2) `public_only()` refuses private addresses; (3) rejects queries (relative resolution drops them).
  - **16c-3 — sign first-party releases and publish the key. Deferred 2026-09-11** ([#93](https://github.com/PromptPasture/jan-klod/issues/93)) until before the first release. `git tag` is empty and there are no releases, so there is no unsigned release in the wild, and a signing identity is a thing to create when it is about to be used. Until it lands, `registry.trusted-keys` has nothing to put in it and every install needs `--allow-unsigned --sha256`. Decided while planning: release will publish per-component files at plain paths.
- **16d — Registry index + `ext search`.** Split four ways ([#53](https://github.com/PromptPasture/jan-klod/issues/53)), one per subtree, because the original scope reached across a generator, `src/core/`, `.github/` and `docs/`. **Narrowed 2026-09-10:** the Configurator half left the slice ([#63](https://github.com/PromptPasture/jan-klod/issues/63)) — `pages/` is a static landing page and no interactive UI exists — so **`ext search` is the capability view** rather than a stand-in for one. **The artefact layout is already fixed by everything below it**, so the index has to describe that rather than a tarball: a publisher serves **four files per component at plain paths** — `<name>.wasm`, `<name>.manifest.toml`, and a `.minisig` for each. 16c-1 verifies that shape, 16c-2 derives companion URLs from it by relative resolution, and 16c-3's release decision commits to producing it. An index describing anything else would disagree with the installer about what a component is.
  - **16d-1 — `index.json` and deterministic generator. Done 2026-09-12** ([#137](https://github.com/PromptPasture/jan-klod/issues/137)).
    `make registry-index` writes name, version, `api-version`, kind, capabilities, description, author, url, `sha256`, signature and size per staged component. The generator is a `jan-klod-core` example rather than a shell script: six of the eleven fields are the manifest's own, and reading them through `ext::list` — the reader boot uses — is what stops the index describing a manifest differently from how the host enforces it. Two things this bullet did not anticipate: there was no committed artefact to diff (`.wasm` gitignored; Rust wasm not byte-stable across machines), so two runs over one directory agreeing is what matters; filesystem order can agree by luck, so sortedness and uniqueness are asserted as properties. All three checks are probed on every run.
  - **16d-2 — `ext search` and `ext list --remote`. Done 2026-09-12** ([#138](https://github.com/PromptPasture/jan-klod/issues/138)).
    `registry.url` names the index; `ext search <term>` prints each hit's identity and then what it asks the host for, and `ext install <name>` resolves through the index. An index is a **directory, not an authority** — the entry's digest becomes the digest the existing install checks, the signature is still verified against `registry.trusted-keys`, and the manifest cross-check is unchanged, so an index that lies produces a refusal rather than an unvouched component. Two decisions worth keeping: a `--sha256` the index contradicts stops the install instead of one winning (two disagreeing claims make neither trustworthy); an `http`/`https` `registry.url` is fetched under the egress policy and anything else is a path — enables offline mirrored index, tested with no socket.
  - **16d-3 — release publishes the index. Done 2026-09-12** ([#139](https://github.com/PromptPasture/jan-klod/issues/139)).
    `release.yml` generates the index on the linux/x86_64 leg — a guest is `wasm32-wasip2`, so four legs would build the same bytes — and the existing `release` job deploys `index.json` and the components it names to GitHub Pages. **Unverified until a tag is pushed**, and Pages is not enabled on the repository yet, so `configure-pages` runs with `enablement: true` and the first release also creates the site.

**Exit gate: passed 2026-09-12**, all four clauses. A component whose manifest omits a capability it imports is refused at boot (16a-2). A tampered download is refused by `ext install` — against a digest and against a signature, the second including bytes tampered in flight (16c-1, 16c-2). An install from a static index fixture works offline (16d-2, `ext_registry::a_name_from_a_local_index_installs_offline`). A WIT major mismatch is a clear error naming both versions (16a-2).

**What is deliberately not done.** 16b-3 (#90) waits for the interface freeze, which needs a first release. Below `1.0` a differing minor is refused by policy, so there is no version pair an adapter would help. 16c-3 (#93) waits for the same release, so `registry.trusted-keys` ships empty and every first-party install still needs `--allow-unsigned --sha256` — which also means the index's `signature` field is empty today and says so rather than pretending. Neither is a gate clause; both are named here so the phase does not read as finished work with a quiet gap.

## Phase 17 — Web client + GUI shell

**Goal:** one front-end codebase serves both the browser and the desktop window.

**13c turned out not to be a prerequisite, and that is the phase's first finding.** The existing **REST + SSE** surface already pushes an `ask`, takes its answer and cancels, so the client was built on that and no socket was needed. [#43](https://github.com/PromptPasture/jan-klod/issues/43) stays deferred, now waiting on evidence rather than on a slice — if the SSE teardown-as-cancel proves wrong for a browser (reconnect churn, lost event ordering, a cancel that must leave the connection up), that is what turns 13c from a refinement into a requirement.

- **17a — Web client. Done 2026-09-12** ([#54](https://github.com/PromptPasture/jan-klod/issues/54)).
  A static, dependency-light TypeScript SPA served by the core at `/`, over **REST + SSE**: sessions, streaming, `ask` answered on a second request, and cancel as a stream teardown the conductor turns into `Flow::Stop`. Four children: [#118](https://github.com/PromptPasture/jan-klod/issues/118) the SPA, [#119](https://github.com/PromptPasture/jan-klod/issues/119) serving it at `/` from `include_str!`-embedded assets, [#120](https://github.com/PromptPasture/jan-klod/issues/120) the npm supply-chain leg, and this record. First first-party TypeScript in the repo, and the supply-chain hygiene the roadmap's ground rule demands is part of it: an exactly-pinned `esbuild`, a committed lockfile, `npm ci --dry-run && npm audit` in CI. The committed bundle gained the drift check its three precedents already had ([#127](https://github.com/PromptPasture/jan-klod/issues/127)) — `src/web`'s own suite runs in CI too, which nothing did when it shipped.
- **17b — Tauri shell. Done 2026-09-12** ([#55](https://github.com/PromptPasture/jan-klod/issues/55)).
  `jan-klod --gui` opens a Tauri 2 window over the same front-end the browser gets — system webview, no bundled browser. Four children: [#141](https://github.com/PromptPasture/jan-klod/issues/141) what Tauri costs, [#142](https://github.com/PromptPasture/jan-klod/issues/142) the window, [#143](https://github.com/PromptPasture/jan-klod/issues/143) packaging, [#144](https://github.com/PromptPasture/jan-klod/issues/144) this record. Four findings: (1) Cost measured first — fails unmodified `deny.toml` on both axes: five MPL-2.0 crates, six unmaintained advisories (+256 packages, 332 CPU-sec, 838 MB target/); deliberate, named crate by crate, scoped to `src/gui`. (2) MPL floor in `wry`, not Tauri — direct webview binding hits same five. (3) Own cargo workspace — 256 packages in `src/core` would reach (406→663), built by every test and CI; outside default path, but `make lockfile`, `deny`, `audit` name it. (4) GUI is not a distribution — `GUI=1` second axis (`make bundle DIST=coding GUI=1`); test fails if `install.sh --gui` and release workflow disagree.

**Exit gate:** a browser and a Tauri window drive a turn with `ask` + cancel from one front-end codebase. **Met for the browser** (17a, `host/tests/it/web_client.rs`). **For the window, met in part and stated precisely rather than rounded up:** the window runs the shipped SPA against a real gateway and authenticates with the seeded token — verified through a recording proxy, `GET /` → `GET /app.js` → `GET /sessions` with a bearer header — and the assets and transport are byte-identical to the browser's. What nobody has done is type into the window, answer an `ask` in it or press cancel in it; that needs a human at a screen. Linux is entirely unexercised. The remaining check is a person on each platform, not more code.

## Phase 18 — Ecosystem ports

**Goal:** MCP and ACP in both directions. The inbound halves exist (`registry-mcp`, `agent-*`); this phase adds the core *as* a server on each. Needs Phase 13a.

- **18a — Core as an MCP server. Done 2026-09-11** ([#56](https://github.com/PromptPasture/jan-klod/issues/56)).
  `jan-klod-gateway mcp` exposes `ask`, `session_list` and `session_get` over MCP stdio. Three findings: (1) No SDK, and no dependency — MCP's stdio transport *is* the framing `rpc` already speaks (newline-delimited JSON-RPC 2.0, frames on stdout, logs on stderr), so this is a method-name adapter over the same envelope. `rmcp`, the official Rust SDK, is async on tokio, and this core is deliberately synchronous; adopting it would have been an architectural change dressed as a convenience. (2) `isError` is a field on a *successful* result, the opposite of `protocol::jsonrpc`'s `Outcome`, which makes result and error mutually exclusive on purpose. Mapping a refused turn onto a JSON-RPC error would make every permission refusal read to an editor as a broken server. (3) The driver had to be headless by construction — stdin carries protocol frames here, so a driver that prompted would read the client's next request as an answer to a confirmation.
- **18b — ACP server side. Done 2026-09-11** ([#57](https://github.com/PromptPasture/jan-klod/issues/57)).
  `jan-klod-gateway acp` serves `initialize`, `session/new`, `session/prompt` with streamed `session/update`, `session/request_permission` and `session/cancel`. Four findings: (1) It is the first surface where the core is a JSON-RPC *client* as well as a server on one pipe. The agent originates `session/request_permission` and blocks on the editor's answer, which is why this needed `rpc`'s reader-thread shape where the MCP port did not. (2) The initial assumption was wrong about the starting point — jan-klod already spoke ACP as a client; `delegate.rs` has a `# Not wired` section saying otherwise, and the only endpoint in it is a fixture's. Nothing here knew ACP's wire format. (3) `stopReason` is not MCP's `isError`. A permission refusal is `end_turn`, because `refusal` means the agent declined the whole exchange and the spec lets an editor discard the user's prompt. A turn that genuinely failed is a JSON-RPC error — the opposite placement from MCP. (4) The disconnect needed no timeout. Over one pipe the channel closes on EOF, so a vanished editor is detected rather than waited out.
- **18c — `registry-mcp` stdio transport. Stands alone; did not gate the phase** ([#58](https://github.com/PromptPasture/jan-klod/issues/58), split into [#109](https://github.com/PromptPasture/jan-klod/issues/109) — long-lived children granted by name, confined, dying with their component — and [#110](https://github.com/PromptPasture/jan-klod/issues/110) — `registry-mcp` over one). MCP servers over a long-lived child process, the Phase 7 "long-lived children" carry-forward, behind the Phase 15 policy. This is the **inbound** direction: the core as an MCP *client*. Both exit-gate clauses describe the core as a *server*, so closing the phase without this slice is not a gap in the gate — it is what the gate says. 18c is also the phase's only WIT contract change, which is why it was sequenced last.
  - **18c-1 done 2026-09-12** (#109): the host capability, with no consumer yet. `host-process` grew `spawn`/`write-stdin`/`read-stdout`/`is-running`/`kill` over a `u32` handle — not a `resource`, because the host must own the child's lifetime regardless, so a resource would have tidied the guest-visible half without discharging the requirement, against two existing handle precedents. The grant, `execution.long-lived`, **names processes**: a guest asks for a name and the host supplies the command, so it is narrower than `execution.enabled`, not a wider version of it. Confinement is not re-implemented — both `exec` and `spawn` go through one `ProcessRunner::prepared`. The child dies with the instance that started it, proven by looking for the pid after the runtime drops rather than by reading the code. The package version moved **0.1.0 → 0.2.0**, the repository's first bump, which exposed two fixtures that had encoded the version they were testing against.
  - **18c-2 done 2026-09-12** (#110): `registry-mcp` speaks MCP over a stdio child, so the inbound half of the ecosystem port reaches the servers most of the ecosystem actually ships. A `Server` carries a `Wire` — HTTP or a child handle — and one dispatch point means `tools/list`, `tools/call` and `reconnect` are transport-agnostic rather than written twice. The framing is newline-delimited JSON-RPC, accumulated until a newline because a read is bounded by `output-cap` and one reply can span several, with an empty read meaning "nothing yet" rather than EOF. A server that never answers is given up on after ten seconds and reported down; the turn still runs. Required a second package bump, **0.2.0 → 0.3.0**, because `mcp-registry-world` did not import `host-process` at all — and the host half of that interface existed only for `tool-*` guests, so the child table became a shared type rather than a second copy of #109's lifetime guarantee.

**Exit gate:** an ACP client fixture runs a turn against the core; an MCP client lists and calls a core-exposed tool — both offline. **Met 2026-09-11**, proven by `acp::a_prompt_streams_an_update_and_ends_the_turn` and `mcp::an_editor_initializes_lists_tools_and_calls_ask` in the host integration suite, which has no network access whatsoever.

## Phase 19 — Terminal client experience

**Goal:** the terminal client stops being a proof that the protocol works and
becomes the thing people use. [#97](https://github.com/PromptPasture/jan-klod/issues/97).

Not one of the vision's six decisions — this phase follows them, and its
premise is narrower: `tui.rs` is 182 lines of a 1689-line client, and the rest
is transport, protocol and CLI. That ratio is also why a rewrite in another
toolkit was rejected; #97 records the measurement.

- **19a — Design system. Done 2026-09-12**
  ([#98](https://github.com/PromptPasture/jan-klod/issues/98), split into
  [#128](https://github.com/PromptPasture/jan-klod/issues/128) and
  [#129](https://github.com/PromptPasture/jan-klod/issues/129)).
  `src/core/ui/src/theme.rs`: one ten-step grey ramp read from either end, four
  accents with one meaning each, the glyph vocabulary as a type, and capability
  detection from an **injected** environment rather than `std::env`. It ships no
  visible feature; it ships the vocabulary the other seven slices are written
  in. The tables live on #97 and the authority is `theme.rs`.
- **19b — Chat surface. Done 2026-09-12**
  ([#99](https://github.com/PromptPasture/jan-klod/issues/99)). Scrolling
  viewport, message blocks, markdown, syntax highlighting — a message is a
  block rather than a line, and a streaming delta cannot yank the view out from
  under someone reading it.
- **19c — Tool blocks. Done 2026-09-12**
  ([#100](https://github.com/PromptPasture/jan-klod/issues/100)). An invocation
  paired with its result in the model, a collapsed line naming what the call
  *did* rather than its JSON, and a unified diff whose signs survive
  monochrome.
- **19d — Composer. Done 2026-09-12**
  ([#101](https://github.com/PromptPasture/jan-klod/issues/101)). Multi-line
  editing with a grapheme-safe caret, history, the `/` command menu rendered
  from the table rather than beside it, and `@` path completion that inserts a
  path and reads nothing.
- **19e — Turn lifecycle. Done 2026-09-13**
  ([#102](https://github.com/PromptPasture/jan-klod/issues/102) — #158 the
  state machine, #159 what `Enter` means, #160 the signals). Two of the three
  things 13b added to the protocol had **no key bound to them at all**:
  `Enter` was guarded on `rx.is_none()`, so while a turn ran the keyboard did
  nothing but answer a confirmation. `Enter` now has four meanings picked by
  the state — send, steer, answer, nothing — extracted into `enter_means`, a
  pure function, because the event loop needs a terminal and a chain of match
  guards is a table nothing can assert. Three signals change together (caret,
  label, hint) because one signal is one thing to miss, and the caret is a
  second `Glyph` rather than a tint since `Mode::Mono` has no tint to spend.
  The spinner advances on the poll tick, not on a delta — `text-delta` is one
  per completion, so an arrival-driven animation would freeze through the wait
  it exists to reassure someone about. `Ctrl+C` and `/cancel` meet at one
  sender, following `/quit`'s existing shape. **Needed #157** first, which gave
  `Transport` `create_session` and a `cancel` with two honest implementations:
  a `turn/cancel` over stdio, a stream teardown over REST.
- **19f — The ask dialog. Done 2026-09-13**
  ([#103](https://github.com/PromptPasture/jan-klod/issues/103)). The permission
  modal, with nothing pre-approved: `App::prompts` is a queue rather than one
  slot, the selection starts on `default`'s own index (found by search, not
  assumed first), `Esc` closes without answering, and the answer sent carries
  the notification's own session rather than the client's current one — the
  bug this slice opened on. A row and its tests are in
  `docs/concepts/security-model.md`.
- **19g — Frame. Done 2026-09-13**
  ([#104](https://github.com/PromptPasture/jan-klod/issues/104)). Header,
  sidebar, status bar, toasts, and #97's responsive rules, built on 19b/19c.
  A new `keymap.rs` table (modelled on `commands.rs`) is what the status bar's
  hints are rendered *from* rather than hand-written, tested by handing
  `keymap::hint` a different table and watching the string move. A new
  `layout.rs::regions(width, height)` is a pure function implementing #97's
  table with no terminal — tested at 120×32, 90×30, 70×24, 50×18 and 60×8 for
  the region set, no overlap, and the "terminal too small" floor. A new
  `sidebar.rs::view` is a projection only (SESSION, THIS TURN, CHANGED, the
  last read off the same `ToolBlock`s the transcript already renders, via
  `diff::counts` rather than guessing `+0 -0` where 19c could not parse a
  diff) — a grep in `tests/sidebar_projection.rs` asserts it names neither
  `Command` nor `Transport`. Toasts (`App::toast_warning`/`toast_error`) are
  driven off the same poll tick the spinner already used, so expiry needs no
  clock; the full text always also lands in the transcript. `App::disconnect`
  stops the spinner without reconnecting, and `Transport::alive` (a
  non-blocking `Stdio::try_wait`) catches a dead gateway even with nothing in
  flight to fail.
- **19h — Dialogs. Done 2026-09-13**
  ([#105](https://github.com/PromptPasture/jan-klod/issues/105)). One modal
  primitive — `tui::render_modal`, centred on `raised()` with a `focus()`
  border — reused by the ask dialog (19f) and four new ones: the session
  switcher, the help overlay, the quit confirm, and the sidebar (19g's) as a
  dialog at widths where its layout leaves it no permanent pane, bound to
  `Ctrl+B` — pointless, and refused, at a width where the pane already exists.
  A grep
  (`tui::every_dialog_shares_one_frame_implementation`) pins that there is
  exactly one such function, the same shape 19a's `Color`-literal check uses.
  The session id stopped being a `&str` held for the event loop's lifetime and
  became owned on `App` (`App::session`/`set_session`), which is what a switch
  or `/new` now replaces. `Transport` gained `session_list` and `session_get` —
  a real implementation on each of `Rest` (`GET /sessions`, `GET /session/:id`)
  and `Stdio` (reads its own answer, valid only with no turn streaming, the
  same caveat `create_session` already carried) — plus `is_stdio`, which is
  what lets the quit confirm word a running turn's fate correctly on each
  transport. Switching is a reload: `session/get`'s messages rebuild the
  transcript (`user`→`Who::You`, `assistant`→`Who::Klod`, `tool`→a `ToolBlock`
  keyed on `tool-call-id` — the wire carries no tool name or failure flag for a
  replayed call, so the id stands in for the name and success is assumed —
  `system`→dropped), and switching mid-turn is refused with a stated reason
  rather than silently abandoning the turn. The help overlay renders from
  `keymap::BINDINGS` grouped by `Context`, so a binding added without a help
  entry is impossible rather than merely undocumented. The quit confirm is
  skipped entirely on a session with no messages sent, otherwise opens with
  cancel selected and names the session id as resumable; quitting mid-turn
  needs a second, explicit confirmation. `/new`, `/sessions` and `/help` moved
  from `Pending` to `Ready` in `commands.rs`, and `exactly_the_commands_that_act_are_ready`
  was updated deliberately. Out of scope, same as the issue: session
  delete/rename (no protocol command) and fork — which is out of scope for a
  different reason than this line used to give. `session/get` returns a `seq`
  per message and it *is* `session/fork`'s `at-seq` (#106), so the protocol
  expresses fork-from-a-point and has since before 19h shipped. What is
  missing is a way for a person to choose the point, and forking mid-session
  is rare enough that nobody has wanted one; if that changes it is a small
  slice against a protocol that already supports it, not a protocol change
  ([#170](https://github.com/PromptPasture/jan-klod/issues/170)).

19a gated the rest; after it, 19b→19c and 19d→19e→19f are two chains that can
run in parallel, and 19g/19h close over both.

**Three things 19a settled that the later slices inherit**, each found by
building it rather than by planning it:

- **There is no terminal background query.** #97's detection chain lists one
  "where the backend supports it". crossterm 0.29, which ratatui 0.30 bundles,
  has no such API — its only OSC sequences are for the clipboard — so the chain
  falls through to dark, which was the documented default anyway.
- **The light theme carries two AA text tiers, not three.** `muted` cannot meet
  4.5:1 as a step shared between both directions of the ramp, and the arithmetic
  says no single step can: 4.5:1 against `chalk` needs a luminance ≤ 0.116,
  against `ink-2` it needs ≥ 0.227. It is therefore not held to a contrast
  floor, and [#135](https://github.com/PromptPasture/jan-klod/issues/135) carries
  the decision about what to do instead.
- **Grey carries structure; colour carries meaning.** The contrast target is
  "any border or glyph *that carries meaning* ≥ 3:1", so the accents and the
  focused border are gated and the grey separators are not — holding an idle
  rule to 3:1 against its own surface would light up every line on a screen that
  is supposed to be quiet.

**Exit gate:** on a 120×32 terminal and on a 60×20 one, a full turn — user
message, streamed answer, two tool calls one of which edits a file, an `ask`
answered, a follow-up sent mid-turn, and a cancel — renders correctly, and the
same run with `NO_COLOR=1` in a 16-colour terminal loses no information.

## Post-v0.1: The Enterprise Box (Phases 26–31)

[Vision — The Enterprise Box](../decisions/2026-09-18-enterprise-box/Vision.md) (2026-09-18) phases
the half of the product Phases 13–25 did not build: the turnkey install for someone who will not
configure anything. Two pillars — *it passes review* (authn/RBAC, audit and export, secrets, policy,
egress, air-gap, signed delivery) and *batteries are included* (providers, memory, skills, MCP
servers, tools).

The rule that shapes all six phases: **enterprise capability ships as an extension**, and the host
gains only the four things it is structurally the only place for — principal extraction, secrets
backends, event-log integrity, and the logging subscriber. RBAC is therefore an interceptor, session
export an extension over `host-agent`, memory a guest over `host-storage`.

Order is by dependency: 26 and 27 are independent of everything and of each other; 28 follows
because it touches the files Phase 21's rename will move; 29 and 30 each hold one slice that waits on
Phase 22's `host-agent`; 31 consumes all of them. Each phase's goal, slices and gate are in the
record; the tracker rows above hold the state. Umbrella and slice issues are unfiled.

## Cross-cutting (continuous, not a phase)

Resource-budget and developer-experience items from the vision, tracked as
issues, not phases: **lazy guest instantiation** (instantiate a component on
first call, not at boot — **#59 done 2026-09-11**, below), a **compiled-component
cache** keyed by component bytes + Wasmtime version + target triple + engine
config (Wasmtime's own built-in cache, not a bespoke `.cwasm` one —
**#60 done 2026-09-11**, below), an **accelerated `host-fs.grep`** (native tree
search behind the existing jail, the guest only shapes the request), the
**Rust extension PDK** (`make ext-new NAME=` template + guest test harness
guide), and **named distributions** in the Configurator (`coding`,
`headless-chat`, `minimal`).

**Distributions 1 done 2026-09-12** ([#114](https://github.com/PromptPasture/jan-klod/issues/114)):
`scripts/distributions/{coding,headless-chat,minimal}/` is a guest list and a
`config.yaml` each, and `make bundle DIST=<name>` builds an archive from the
pair — with no `DIST` the target is unchanged. Distributions name what an
install is *for*, which the old `tui`/`gui`/`full` presets mixed with how the
user looks at the runtime. `docs_match_config` now checks every shipped config
rather than only the root one, per config rather than pooled. **Distributions 2 done the same day**
([#115](https://github.com/PromptPasture/jan-klod/issues/115)):
`scripts/install.sh --dist <name>` picks one, defaulting to `coding`, and
`release.yml` publishes all three per platform — four jobs producing three
archives each rather than a 4×3 matrix, because the expensive half of a release
job is per platform and half the runners bill at 10×. The three places that
name distributions — the definitions, the installer, the workflow — are held to
one set by a test, since a disagreement between any two is a 404 at a user
after a release. **Distributions 3 done the same day**
([#116](https://github.com/PromptPasture/jan-klod/issues/116)):
`configurator.md` describes the three as built rather than planned — the
client-named `tui`/`gui`/`full` preset table is gone, since that is exactly what
distributions replace — and the quickstart and landing page offer all three.
**Named distributions are complete.** Running those pages' commands rather than
transcribing them surfaced that the pre-existing `curl … | sh` instruction fails
for want of any release at all, filed as
[#126](https://github.com/PromptPasture/jan-klod/issues/126).

**PDK 1 and 2 done 2026-09-12** ([#111](https://github.com/PromptPasture/jan-klod/issues/111),
[#112](https://github.com/PromptPasture/jan-klod/issues/112)): `make ext-new
NAME=… KIND=…` writes a crate that is registered in the workspace and in
`GUESTS`, formatted, and compiles — one generator with a per-kind table, since
what differs between kinds is a world, a trait and a type list and everything
else is identical. `KIND` is `provider | tool | interceptor | registry-skills |
registry-mcp`: `registry` was two worlds all along, and **`agent` is not a kind**
— there is no agent guest world in `wit/` and no `("agent", _)` arm in
`Runtime::boot`, so such a crate would compile against nothing and never load.
`src/extensions/tool-hello` is the committed output, kept byte-identical to the
generator by `scripts/ext-new-selftest.sh`, and
`host/tests/it/generated_guest.rs` is the copyable test that loads it and calls
it — probed three ways, because a harness that loads nothing passes as quietly
as one that works. **PDK 3 done the same day** ([#113](https://github.com/PromptPasture/jan-klod/issues/113)):
`docs/guides/writing-an-extension.md` walks the path end to end, including the
install refusal an author hits first — signatures verify against
`registry.trusted-keys`, which ships empty, so `--allow-unsigned --sha256` is
the way through until #93 publishes a key. Every command on that page was run
rather than transcribed, which is how its own first command turned out to fail.
**The PDK is complete.**

**#59 done 2026-09-11:** `tool-*`/`registry-*`/`agent` guests compile at boot
(unchanged) but instantiate (`Store` + `init` + `start`) only when the fleet
is first asked for something — its metadata (`select-tools` advertising) or
an `invoke`, whichever comes first; providers and interceptors stay eager,
not negotiable. The boot-plan/`verify`-offline path (`Runtime::start_all`, the
default no-subcommand invocation) no longer instantiates the lazy categories
at all; `verify`/`verify --live` are unaffected (`Runtime::start_all_eager`
keeps their original, fully-eager behaviour, since proving instantiate+start
work is their entire purpose). Measured honestly rather than assumed: on the
shipped `config.yaml` (3 tools, `tool-selector` enabled) neither RSS-after-boot
nor time-to-first-prompt moved outside run-to-run noise, before or after,
because `tool-selector` needs every tool's metadata before turn one regardless
— the real, identified reason is that compiling each guest (Cranelift JIT,
unchanged by this issue) costs more than instantiating one at this repo's
guest sizes, confirmed by re-running the same comparison on a synthetic
14-instance config with all eight available tools enabled. The catalog
option the issue raised — sourcing `select-tools`'s advertisement from the
manifest instead of the guest, for real per-tool laziness — was decided
against for this slice: it would need the manifest to carry a tool's
model-facing name/description/schema, producible only by *running* `meta()`
at manifest-generation time, and nothing today catches that manifest and the
guest's own `meta` drifting apart (16a's `inspect` cross-check covers
host-capability imports, not tool metadata). Left as a follow-up, not
half-built. Full detail, the measured numbers, and the mechanism (
`LazyToolFleet`/`LazyRegistryFleet`) are in
[the changelog](../changelog.md#2026-09-11).

**#60 done 2026-09-11:** evaluated Wasmtime's built-in cache first, as the
issue asked, and it meets the need — adopted instead of a hand-rolled `.cwasm`
cache, and stopped there. Checked against 46.0.3 (this workspace's pinned
version): the issue's named API, `Config::cache_config_load*`, no longer
exists there; the current shape is `Cache::from_file`/`CacheConfig` plus
`Config::cache(Some(cache))`, and it already caches components (not only core
modules), keys on component bytes + target triple + compiler/ISA flags +
Wasmtime version (`HashedEngineCompileEnv`, `wasmtime-46.0.3/src/compile/
code_builder.rs`), and treats a corrupt or foreign artefact as a miss, never
an error — every property the issue's fallback would otherwise have had to
build by hand. `storage.cache-dir` (optional; defaults to `wasmtime-cache`
beside `config.yaml`) is the one new config surface; the directory is created
user-private (`0700` on unix) since a hit is deserialized as native code, not
re-verified — a new row in
[Security model](security-model.md#capabilities) names the test. Proven live
against the shipped `config.yaml`: a fresh boot logs
`wasmtime compile cache: 0 hit(s), 9 miss(es)`, a second boot against the same
directory logs `9 hit(s), 0 miss(es)` — not inferred from a timing. Measured,
same methodology as #59: cold ~0.20s / ~48MB peak RSS vs warm ~0.02s / ~24MB —
roughly 10x less boot time and half the peak RSS, the win #59's honest
"no difference" measurement said would need this issue. Full detail, the
key-completeness argument, and the numbers are in
[the changelog](../changelog.md#2026-09-11).



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

## Curated memory — answered by Phase 30

The file-workspace tier (files, processes, and the `tool-*` fleet) was scoped as **Phases 7–8**, and
one item from that original scope stayed out as a deliberate open question: **long-term / curated
memory.** The standard it was parked under was *decide if before how* — do not add a
memory-curation interface speculatively.

**Answered 2026-09-18** in [Vision — The Enterprise Box](../decisions/2026-09-18-enterprise-box/Vision.md),
decision 7, and split rather than answered whole. The **episodic half ships**: a thin `tool-memory`
guest that stores and recalls facts over the `host-storage` namespace Phase 3 already delivered,
needing no new contract — Phase 30a. The **curation half does not**: semantic search, consolidation
and working/episodic tiering stay out, because they belong to an MCP server or a third party and
`registry-mcp` already reaches both. That is what the parked question was protecting against, and it
still holds — no memory-curation interface is added, then or now.
