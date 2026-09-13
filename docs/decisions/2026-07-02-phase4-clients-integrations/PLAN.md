# Phase 4 Plan — Clients & Integrations

Execution checklist for Phase 4. Update flags here and in [Status tracker](../../concepts/roadmap.md#status-tracker).

**Prerequisite:** Phase 3 done (2026-07-02) — loop runs, state durable, core reachable via `jan_klod_core::serve`.

References: [architecture.md → UIs](../../concepts/architecture.md#user-interfaces-separate-clients) · [`agent-delegate.wit`](../../../wit/agent-delegate.wit) · Phase 3 REST contract.

## Goal

Human- and agent-facing surfaces: **UI client** (separate process over REST), **`host-socket`** + 
first `chat-*` integration (headless chat), **`agent-*` ACP delegation** (core delegates to/called by agents).

## Architecture invariants

- **UIs/clients are separate processes** connecting over REST (LSP/server model). Client contract: Phase 3 REST.
- **`chat-*` and `agent-*` are sandboxed WASM extensions** driving loop; get long-lived connectivity via **`host-socket`**.
- **ACP both ways** — client (`agent-*` calls other agents) and server (core callable by ACP orchestrators over REST).

## TBD resolutions

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| UI client shape (v1) | Slice 4a | CLI/REPL client first, TUI (`ratatui`) layered after | Proves separate-over-REST; REST live. TUI is presentation. |
| TUI toolkit | Slice 4a | `ratatui` (+ `crossterm`) | De-facto Rust TUI standard. |
| GUI shell | later | `Tauri`/WebView | Off v1 path; browser reaches core via REST. |
| `host-socket` | Slice 4b | Host-owned socket guest opens (`connect`/`send`/`recv`), host holds fd | Sandbox no sockets; mirrors `host-http`/`host-serve`. Confirm vs. `chat-telegram` need. |
| First `chat-*` | Slice 4b | `chat-telegram` (long-poll via `host-http` suffices) | Unlocks headless Pi/container; Telegram API long-poll-friendly. |
| ACP transport | Slice 4c | REST both directions; `agent-delegate` guest outbound | Reuses Phase 3; no new transport. |

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Slice | Flag |
|---|---|
| 4a — UI client (CLI/REPL first, then TUI) | `done` |
| 4b — `host-socket` + `chat-telegram` | `done` |
| 4c — `agent-*` ACP delegation (both directions) | `done` |
| 4d — Exit gate | `done` |

---

## Slice 4a — UI client

- [x] **Thin client binary** — `jan-klod-ui` (separate crate, no core/Wasmtime): `send_turn(addr, session, message)` POSTs to running core, returns answer. REPL over it (shared session). 3 unit tests + `roundtrip` + e2e smoke. `make chat`.
- [x] **TUI** — `jan-klod-ui tui [addr] [session]` layers `ratatui` over `send_turn`: scrollable transcript + input. Pure `app::App` model (typing/backspace/submit/record), 4 tests. Render loop thin. GUI deferred.

**Done:** client drives `jan-klod serve` over REST, holds multi-turn conversation (REPL or TUI).

---

## Slice 4b — `chat-telegram`

- [x] **Transport need resolved: no `host-socket` v1.** Telegram Bot API is outbound HTTP only (`getUpdates` + `sendMessage`); existing HTTP covers it. `host-socket` deferred (YAGNI; revisit if platform needs persistent inbound).
- [x] **Build `chat-telegram`** — `jan_klod_core::telegram`: `parse_updates`/`next_offset` (pure) + `poll_once()` fetches updates, drives each message through loop (chat id = session), sends answer. Host-side; HTTP injected as `Fetch` closure for offline testing. Launchable: `jan-klod telegram`.

**Done:** `telegram.rs` — canned inbound message drives `poll_once` cycle through guests, `sendMessage` carries answer, offline.

---

## Slice 4c — `agent-*` ACP delegation

- [x] **Outbound** — `AgentDelegate` plugs into `ToolInvoker` seam. Model emits `delegate` tool call; `AgentDelegate` resolves ACP endpoint, forwards task over injected `AgentTransport`, returns remote answer. 5 unit tests. *(Concrete ACP-over-HTTP `AgentTransport` + wiring into `build_agent` is remaining glue.)*
- [x] **Inbound** — no new code: core callable by ACP orchestrator over REST (`serve`); orchestrator POSTs task, reads answer.

**Done:** delegated task round-trips offline; `AgentDelegate.invoke` forwards to stub, returns answer.

---

## Slice 4d — Exit gate

- [x] Integration tests: UI client over REST (`jan-klod-ui` `roundtrip`) and Telegram message drives turn + reply (`telegram.rs`), both offline.
- [x] CI — `make phase4-gate`, wired into harness job.
- [x] Mark Phase 4 `done` in [roadmap.md](../../concepts/roadmap.md). Carried-forward: TUI GUI, concrete ACP-over-HTTP `AgentTransport`, `host-socket`.

**Done:** `make phase4-gate` CI green; `roadmap.md` updated. ✓

## Cross-cutting

- Strict clippy; `cargo test` green per PR; supply-chain gates to new deps.
- Client ↔ core on REST contract; no bespoke transport.
