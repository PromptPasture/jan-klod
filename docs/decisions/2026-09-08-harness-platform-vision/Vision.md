---
type: decision
title: Vision — Harness as a Platform
description: jan-klod after v0.1.0 as an agent runtime (kernel + distributions + clients) rather than one coding agent; the five-layer architecture; six direction-setting decisions (client protocol as a contract, OS-level effect sandbox, event-sourced session log, capability manifest + signed registry, GUI as a Tauri shell over the web client, MCP + ACP in both directions), the prior art each borrows from, and the mapping onto roadmap Phases 13–18.
method: comparative analysis
date: "2026-09-08"
status: accepted — phased as roadmap Phases 13–18, tracked in GitHub Issues
tags: [vision, product, architecture, protocol, sandbox, event-log, registry, clients, prior-art, roadmap]
created: 2026-09-08
updated: 2026-09-08
related:
  - ../../concepts/roadmap.md
  - ../../concepts/architecture.md
  - ../../concepts/security-model.md
  - ../../concepts/contracts.md
  - ../../concepts/configurator.md
  - ../2026-07-01-thin-loop-interceptors/BRAINSTORM.md
  - ../2026-07-02-phase4-clients-integrations/PLAN.md
  - ../2026-07-02-phase7-file-workspace-substrate/PLAN.md
---

# Vision — Harness as a Platform

## Goal

This record answers how to build a harness that:

1. consumes few resources (single native binary, no bundled JS runtime),
2. has a small, stable kernel with flexible extensions,
3. runs plugins safely,
4. is usable via TUI, GUI, and web page from the same core,
5. runs on Linux, macOS, and Windows.

It fixes the shape—layers, boundaries, borrowings—and six direction-setting decisions. Sequencing is in the [roadmap](../../concepts/roadmap.md) as Phases 13–18, tracked in GitHub Issues.

## Context

v0.1.0 is complete (Phases 1–12). What exists already answers structurally:

- **Rust + Wasmtime core**: the loop is mechanism; each decision is a sandboxed [`interceptor-*` guest](../2026-07-01-thin-loop-interceptors/BRAINSTORM.md).
- **WASM Components against WIT contracts**, polyglot by design, first-party Rust.
- **Separate client processes** over REST + SSE; TUI and Telegram exist; GUI/ACP to follow.
- **Default-deny substrates**: `host-fs` (path-jailed), `host-process` (bounded exec).
- **Curated bundles** via the [Configurator](../../concepts/configurator.md).

This skeleton was reached independently by Codex, OpenCode, and Zed—convergence is the strongest validation.

The gap is not in the kernel. Four areas need it: client protocol lacks first-class contract status; sandbox isolates code but not `host-process` effects; sessions are transcripts, not event streams clients consume; and extension distribution (manifest/provenance/registry) doesn't exist.

## Product shape — kernel, distributions, clients

Position jan-klod as an **agent runtime**, following Linux distributions and Neovim plugins. Three audiences:

| Audience | Gets | Prior art |
|---|---|---|
| **User** | one binary + a distribution (curated extensions + config): `coding`, `headless-chat`, `minimal`. | Linux distros; Configurator |
| **Extension author** | per-language PDK, `ext new`, local registry, capability manifest, guest test harness | Extism, Zellij, Zed |
| **Integrator** | versioned protocol + headless SDK for IDE/CI embedding | Codex, Claude Agent SDK, ACP |

Topologies share codebase, differing only in config:

- *Local, single user* — client spawns core on demand; local socket with per-launch token.
- *Headless* (Pi, container) — core + `chat-*` channel, no UI.
- *Remote server* — core + web client + auth; same protocol.

OpenCode proves these work together (serve + TUI in one binary).

## Architecture — five layers

```
L4 Clients        TUI (ratatui) | GUI (Tauri = the same web front-end) | Web | IDE via ACP | chat channels
L3 Protocol       ONE versioned schema of commands/events; transports: stdio JSON-RPC, WebSocket, SSE
L2 Extensions     provider | tool | interceptor | registry | agent | channel   (WASM Components, polyglot)
L1 Capabilities   fs | process | http | storage | secrets | ask | clock          (host-provided, default-deny, granted by manifest)
L0 Kernel         lifecycle | capability broker | loop conductor | session event log | SQLite | protocol server
```

- **L0 Kernel stays boring.** Mechanism, no policy. Two additions: protocol server and session event log (everything smart lives in L2).
- **L1 Capabilities are syscalls—default-deny, granted per-extension.** `secrets` and `clock` (not yet present) complete the picture but are not scheduled. *(Updated 2026-09-18: `secrets` is scheduled as Phase 27 in [The Enterprise Box](../2026-09-18-enterprise-box/Vision.md); `clock` is still unscheduled and is an open question there.)*
- **L2 Extensions** retain existing taxonomy; `channel-*` covers `chat-*` + e-mail/webhook.
- **L3 Protocol** is the *ABI for clients* (see decision 1).
- **L4 Clients** are event-log projections; none hold core state.

## Cross-cutting principles

**Contracts are versioned like an ABI.** WIT follows semver; extensions declare their `api-version`; the core maintains N-1 compatibility via adapters, not breaking releases. Contracts remain free to change before v1.0; freeze-point naming must accompany the release plan.

**Capability manifest travels with the component.** Extensions pre-declare needs; users approve at install; hosts enforce. Manifests must ship with `.wasm` so registries can show required capabilities before download.

**Two sandboxes, not one.** WASM isolates code; `host-process` requires OS-level isolation. Codex and Claude Code converged: Seatbelt (macOS), Landlock + seccomp (Linux, with policy objects). Windows lacks maturity; ship approval-only mode and be transparent about the gap.

**The event log is the source of truth.** Sessions are append-only event sequences; clients are projections. This enables resume, fork, share-links, replay, and audit; Codex uses this model. SSE events exist; what's needed is canonizing them as the storage format.

**Protocol over REST.** JSON-RPC with stdio/WebSocket/SSE transports (ACP-compatible) lets editors connect without bespoke plugins and unifies GUI/Web frontends.

**Resource budget is a requirement.** Lazy guest instantiation; AOT component cache; pooling allocator; single binary, no Node; SQLite. GUI via Tauri webview, not bundled browser. WASM I/O overhead (tree-wide grep) mitigated by accelerated host capabilities (e.g., `host-fs.grep`) implemented natively.

## Prior art — what to borrow

| Source | Borrow |
|---|---|
| **Codex** (openai/codex, `codex-rs`) | `protocol` crate as the client contract; `app-server` JSON-RPC; OS sandbox with policies; approval modes; `exec` non-interactive mode; the harness itself exposed as an MCP server |
| **OpenCode** (sst/opencode) | server / TUI as separate processes; models.dev provider catalogue; LSP diagnostics fed back to the agent; session share links |
| **Claude Code** | lifecycle hooks (map onto interceptor phases); skills; subagents; plugins + marketplace; headless Agent SDK |
| **Zed** | WASM Component extensions against a versioned WIT API; reviewed extension registry |
| **Goose** (Block) | MCP as the *external* extension protocol — an ecosystem without first-party authors |
| **Extism, Zellij** | per-language PDK; plugin developer experience; plugin test harness |
| **Aider** | tree-sitter repo map as a context strategy; edit-format reliability benchmark |
| **Cline** | workspace checkpoints for rollback (fits COW in `host-fs`) |
| **oh-my-pi** (MIT Pi fork) | hashline edit format; catalogue as the context-budget source; decomposition kept driver-side |

Not reviewed: "DeepSeek harness" — no reliable knowledge of which artefact is
meant; left as an open question.

## Decisions

1. **Client protocol as a first-class contract (own schema, version, tests).** JSON-RPC over stdio/WebSocket/SSE; ACP as external form. REST becomes a projection, not the definition. → **Phase 13.**
2. **OS-level sandbox for `host-process`.** Seatbelt/Landlock+seccomp with policy objects; approval-only on Windows (surfaced to user). → **Phase 15.**
3. **Session stored as event log.** The conductor's `EventSink` is canonical; transcript/REST/SSE are projections. → **Phase 14.**
4. **Capability manifest + signed registry.** Manifests ship with components; provenance verified on install; registry shows capabilities before download. → **Phase 16.**
5. **GUI is a Tauri shell around the web client**, not a separate codebase. → **Phase 17.**
6. **MCP and ACP in both directions.** MCP provides tools (add core as MCP server); ACP provides editors (add server side). Neither replaces WASM extensions—MCP is *external*, WASM is *internal*. → **Phase 18.**

Standing rules unchanged: core is Rust only; first-party extensions default to
Rust; the loop conductor stays in core; storage stays host-side.

## Phase mapping

Order is by dependency: protocol (13) enables web/ecosystem (17–18); event log (14) enables resume/fork/replay; sandbox (15) and registry (16) run parallel with 13–14.

| Phase | Decision | Tracking | Why in this order |
|---|---|---|---|
| 13 — Client protocol | 1 | [#35](https://github.com/PromptPasture/jan-klod/issues/35) | Everything client-facing after it builds on it |
| 14 — Event-sourced session log | 3 | [#36](https://github.com/PromptPasture/jan-klod/issues/36) | Small, host-only, unlocks resume/fork/replay; independent of 13 |
| 15 — OS-level effect sandbox | 2 | [#37](https://github.com/PromptPasture/jan-klod/issues/37) | Largest security gap; independent, platform-by-platform |
| 16 — Capability manifest + signed registry | 4 | [#38](https://github.com/PromptPasture/jan-klod/issues/38) | Independent; prerequisite for a public extension ecosystem |
| 17 — Web client + GUI shell | 5 | [#39](https://github.com/PromptPasture/jan-klod/issues/39) | Needs 13 (WebSocket transport) |
| 18 — Ecosystem ports (MCP server, ACP server) | 6 | [#40](https://github.com/PromptPasture/jan-klod/issues/40) | Needs 13 (both are adapters over the protocol) |

Cross-cutting items that are issues but not phases: lazy guest instantiation,
AOT component cache, accelerated `host-fs.grep`, the Rust extension PDK
(`ext new` + guest test harness), and naming the distributions in the
Configurator. See the roadmap's
[cross-cutting section](../../concepts/roadmap.md#cross-cutting-continuous-not-a-phase).

## Risks

- **Component Model async is maturing.** Sync Wasmtime is the baseline; revisit at WASI 0.3 stabilization.
- **Polyglot is expensive** (CI/supply-chain); Rust-by-default stays; TinyGo remains proof. Web client is first repo TypeScript; must stay dependency-light.
- **Ecosystem chicken-and-egg.** MCP bridge is the fast path; WASM registry grows behind it.
- **Windows lacks sandbox parity.** Ship approval-only mode and be transparent about the limitation.
- **WASM I/O overhead** for tree-wide operations—mitigated by accelerated host capabilities.

## Open questions

- Protocol shape: adopt ACP wholesale as the *internal* protocol, or keep an own
  schema with an ACP adapter? Lean: own schema (ACP does not cover extension
  lifecycle, capability grants, or the `ask` round-trip fully), ACP adapter.
  Resolved in Phase 13, slice a.
- Event-log schema stability: does the log need its own version independent of
  the protocol version? Lean: yes — stored logs outlive client versions.
- Registry hosting: GitHub-releases-as-registry (Zed style) vs. an index service.
  Lean: static index over HTTP first (the Configurator already assumes it).
- Which OS sandbox implementation to reuse rather than write.
- What "DeepSeek harness" refers to and whether it adds anything above.
