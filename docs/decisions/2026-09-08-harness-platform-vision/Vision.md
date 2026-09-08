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

Answer, at the level of *product and architecture* rather than code, how a harness
should be built so that it

1. consumes few resources (single native binary, no bundled JS runtime),
2. has a small, stable kernel with a very flexible extension system,
3. runs plugins in a safe environment,
4. is usable through a TUI, a GUI and a web page from the same core,
5. runs on Linux, macOS and Windows.

This record fixes the *shape* — layers, boundaries, what is borrowed from whom —
and six direction-setting decisions. Sequencing lives in the
[roadmap](../../concepts/roadmap.md) as Phases 13–18; work items live in GitHub
Issues (one umbrella per phase, one issue per slice). Library choices are made
just-in-time inside each phase, as before.

## Context

v0.1.0 is complete (roadmap Phases 1–12). What exists already answers most of the
five asks structurally:

- **Rust + Wasmtime core, zero agent behaviour** — the loop is mechanism, every
  decision is a sandboxed `interceptor-*` guest
  ([thin loop](../2026-07-01-thin-loop-interceptors/BRAINSTORM.md)).
- **Extensions are WASM Components against WIT contracts**, polyglot by design,
  first-party in Rust.
- **Clients are separate processes** over a host-side REST + SSE surface
  ([Phase 4](../2026-07-02-phase4-clients-integrations/PLAN.md)); a `ratatui`
  TUI and a Telegram channel exist; GUI (Tauri) and ACP transport are carried
  forward.
- **Default-deny substrates** `host-fs` (path-jailed) and `host-process`
  (bounded exec) ([Phase 7](../2026-07-02-phase7-file-workspace-substrate/PLAN.md)).
- **Curated bundles** via the [Configurator](../../concepts/configurator.md).

The same skeleton was reached independently by Codex (Rust `core` + `protocol`
crate + several frontends), OpenCode (server + thin TUI/web clients) and Zed
(WASM Component extensions with a versioned WIT API). That convergence is the
strongest evidence the foundation is right.

The gap between "v0.1.0" and "a platform" is therefore **not in the kernel**. It
is in four places: the client protocol is not yet a first-class contract; the
sandbox isolates extension *code* but not the *effects* of `host-process`; the
session is stored as a transcript rather than as the event stream the clients
already consume; and extension distribution (manifest, provenance, registry) does
not exist.

## Product shape — kernel, distributions, clients

Position jan-klod as an **agent runtime**, not "another coding agent". The
analogy is the Linux kernel and its distributions, or Neovim and its plugins.
That gives three audiences, each with its own surface:

| Audience | Gets | Prior art |
|---|---|---|
| **User** | one binary + a *distribution* (curated extension set + `config.yaml`): `coding`, `headless-chat`, `minimal`. The Configurator already builds these; the vision names them. | Linux distros; Configurator |
| **Extension author** | a PDK per language, `ext new`, a local registry, a capability manifest, a guest test harness | Extism, Zellij, Zed |
| **Integrator** | a versioned protocol + headless SDK to embed the core in an IDE, CI, or another orchestrator | Codex `app-server`, Claude Agent SDK, ACP |

**Deployment topologies** share one codebase and differ only in config:

- *Local, single user* — the client spawns the core on demand; local socket with a
  per-launch token.
- *Headless* (Raspberry Pi, container) — core + a `chat-*` channel, no UI client.
- *Remote server* — core + web client + authentication; same protocol.

OpenCode's `serve` mode and TUI living in one binary shows these do not conflict.

## Architecture — five layers

```
L4 Clients        TUI (ratatui) | GUI (Tauri = the same web front-end) | Web | IDE via ACP | chat channels
L3 Protocol       ONE versioned schema of commands/events; transports: stdio JSON-RPC, WebSocket, SSE
L2 Extensions     provider | tool | interceptor | registry | agent | channel   (WASM Components, polyglot)
L1 Capabilities   fs | process | http | storage | secrets | ask | clock          (host-provided, default-deny, granted by manifest)
L0 Kernel         lifecycle | capability broker | loop conductor | session event log | SQLite | protocol server
```

- **L0 Kernel stays boring.** Mechanism, no policy — unchanged. Two additions
  only: the *protocol server* (today the REST surface) and the *session event
  log* (today the transcript store). Everything "smart" lives in L2.
- **L1 Capabilities are the syscalls.** Each is default-deny and granted per
  extension. `secrets` (API keys handed to a provider without the guest ever
  reading config plaintext) and `clock` (bounded timers for polling channels) are
  the two not yet present; they are listed to complete the picture, not scheduled.
- **L2 Extensions** keep the existing taxonomy. `channel-*` is the `chat-*`
  family under a name that also covers e-mail/webhook inbound.
- **L3 Protocol** is the *ABI for clients* — see decision 1.
- **L4 Clients** are projections of the event log over L3. None holds state the
  core does not.

## Cross-cutting principles

**Contracts are versioned like an ABI.** WIT packages follow semver; an extension
declares the `api-version` it was built against; the core keeps N-1
compatibility through adapters instead of breaking releases. This is what makes
the kernel *stable* in the eyes of an ecosystem (Zed's model). Contracts are
still free to change before the first public release; the freeze point must be
named when that release is planned.

**Capability manifest travels with the component.** An extension declares what it
needs *before* load; the user approves at install; the host enforces (Deno /
Android permission model). Today the grants live in `config.yaml`; the manifest
must ship beside the `.wasm` so a registry can show "this tool asks for network"
before download.

**Two sandboxes, not one.** WASM isolates extension *code*. `tool-shell` via
`host-process` runs an arbitrary command on the host — a second boundary that
WASM does not cover. Codex and Claude Code converged on the same answer: Seatbelt
on macOS, Landlock + seccomp on Linux, with a *policy object* (writable paths,
network on/off, approval mode). Windows has no mature equivalent: AppContainer or
a restricted token for a baseline; where unavailable, an explicit
**approval-only** mode that tells the user effects are not isolated.

**The event log is the source of truth.** A session is an append-only sequence
of events — message, tool call, tool result, ask, answer, text delta. Clients are
projections. This gives resume, fork, share links (OpenCode), replay for tests,
and audit for free; Codex is built on exactly this Submission/Event pair. The SSE
events already exist; what is missing is canonising them as the *storage* format.

**Protocol over REST.** A REST resource model serves one client well and three
badly. One JSON-RPC protocol with stdio, WebSocket and SSE transports, ACP-
compatible so editors connect without a bespoke plugin, lets GUI and Web be one
front-end with two transports.

**Resource budget is a requirement, not an outcome.** Lazy instantiation of
guests on first use; an AOT cache of compiled components for fast start;
Wasmtime's pooling allocator; one binary, no Node in-process; SQLite. GUI through
the system webview (Tauri) instead of a bundled browser. TUI-first means the heavy
UI is never loaded unless asked for. I/O-heavy tools (tree-wide grep) are slower
in WASM than native `ripgrep`; the answer is not to abandon the sandbox but to add
*accelerated host capabilities* (e.g. `host-fs.grep`) implemented natively, with
the guest only shaping the request.

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

1. **The client protocol becomes a contract of the same rank as WIT.** Own
   schema, own version, compatibility tests; JSON-RPC over stdio / WebSocket /
   SSE; ACP as the external form. REST becomes one projection of it, not the
   definition. *Supersedes* the Phase 3 lean "UI always connects via REST".
   → **Phase 13.**
2. **A second, OS-level sandbox for `host-process`.** Seatbelt / Landlock +
   seccomp behind a policy object; honest degradation on Windows (approval-only,
   surfaced to the user). Closes the "long-lived children / OS isolation"
   carry-forward from Phase 7. → **Phase 15.**
3. **The session is stored as an event log.** The conductor's `EventSink` stream
   is the canonical record; transcript, REST resources and SSE are projections.
   → **Phase 14.**
4. **Capability manifest + signed registry.** Manifest ships with the component;
   provenance (signature, checksum, WIT-version check) is verified on install;
   registry metadata shows requested capabilities before download. Extends the
   Phase 5 "staging" carry-forward. → **Phase 16.**
5. **GUI is a Tauri shell around the web client**, not a third client codebase.
   → **Phase 17.**
6. **MCP and ACP in both directions are the ecosystem ports.** MCP gives tools on
   day one (already inbound via `registry-mcp`; add the core *as* an MCP server);
   ACP gives editors on day one (already the `agent-*` client side; add the
   server side). Neither replaces WASM extensions — MCP is *external* extension,
   WASM is *internal*. → **Phase 18.**

Standing rules unchanged: core is Rust only; first-party extensions default to
Rust; the loop conductor stays in core; storage stays host-side.

## Phase mapping

Order is by dependency, not by value: the protocol (13) is what the web client
(17) and the ecosystem ports (18) are built on; the event log (14) is what
resume/fork and the protocol's replay need; the sandbox (15) and the registry
(16) are independent of both and can run in parallel with 13–14.

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

- **Component Model async is still maturing.** Sync Wasmtime remains the right
  baseline; revisit when WASI 0.3 stabilises.
- **Polyglot extensions are expensive** in CI and supply chain — Rust-by-default
  stays; the TinyGo gate remains the polyglot proof. The web client (Phase 17)
  is the first first-party TypeScript in the repo; it must stay dependency-light
  for the same reason.
- **Extension ecosystem chicken-and-egg.** The MCP bridge is the only fast
  answer; the WASM registry grows behind it.
- **Windows is the weakest platform for the effect sandbox.** Ship approval-only
  there first and say so, rather than claim parity.
- **WASM I/O overhead** for tree-wide operations — mitigated by accelerated host
  capabilities, at the cost of a slightly larger L1.

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
