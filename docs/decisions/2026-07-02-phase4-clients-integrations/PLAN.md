# Phase 4 Plan — Clients & Integrations

Living execution checklist for Phase 4 of the [Roadmap](../../concepts/roadmap.md).
Update the flags here and in the roadmap [Status tracker](../../concepts/roadmap.md#status-tracker)
as work proceeds.

**Prerequisite:** Phase 3 done (2026-07-02) — the loop runs, state is durable, and
core is reachable over the host-side REST surface (`jan_klod_core::serve`,
launchable via `jan-klod serve`).

Relevant background:
[architecture.md → User interfaces](../../concepts/architecture.md#user-interfaces-separate-clients) ·
[Transport](../../concepts/architecture.md#transport) ·
[`wit/agent-delegate.wit`](../../../wit/agent-delegate.wit) · Phase 3 REST contract
(`POST {session?, message}` → `{answer, agentic}`).

## Goal

The human- and agent-facing surfaces: a **UI client** (a separate process that
drives core over REST), a **`host-socket`** capability + a first `chat-*`
integration (unlocking headless chat-only access), and **`agent-*` ACP
delegation** (core delegating to, and being called by, other agents).

## Architecture invariants carried in

- **UIs/clients are separate processes**, not extensions — they connect over the
  REST surface (the LSP/server model; core is the server). The client contract is
  the Phase 3 REST endpoint.
- **`chat-*` and `agent-*` are sandboxed WASM extensions** that *drive the loop*;
  they get long-lived connectivity through **`host-socket`** (the inbound/outbound
  socket capability the sandbox otherwise denies).
- **ACP both ways** — as a client (`agent-*` extensions call other agents) and as a
  server (core is callable by other ACP orchestrators, over the REST surface).

## TBD resolutions (recommended leans — confirmed at the slice that needs each)

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| UI client shape (v1) | Slice 4a | A thin **CLI/REPL client** first (a `jan-klod` client binary that POSTs turns to a running core), TUI (`ratatui`) layered on after | Proves the separate-client-over-REST model with no toolkit risk; the REST contract is already live. TUI is presentation over the same transport. |
| TUI toolkit | Slice 4a (TUI step) | `ratatui` (+ `crossterm`) | De-facto Rust TUI standard; the architecture already names it. |
| GUI shell | later | `Tauri` / WebView | Off the v1 path; a browser already reaches core via REST. |
| `host-socket` shape | Slice 4b | A host-owned long-lived socket the guest opens (`connect`/`send`/`recv`/`close`), host holding the fd | Sandbox has no sockets; mirrors `host-http`/`host-serve` inversion. Confirm against a real `chat-telegram` long-poll/websocket need. |
| First `chat-*` | Slice 4b | `chat-telegram` (long-poll via `host-http` if it suffices, else `host-socket`) | Unlocks the headless Raspberry-Pi/container use case; Telegram's bot API is long-poll-friendly, which may not even need `host-socket` v1. |
| ACP transport | Slice 4c | ACP over the existing REST surface both directions; `agent-delegate` guest drives outbound | Reuses the Phase 3 surface; no new transport. |

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

- [x] **Thin client binary** — `jan-klod-ui`, a **separate crate/process** that
  depends on neither core nor Wasmtime: `send_turn(addr, session, message)` `POST`s a
  turn to a running core and returns the answer; the binary is a REPL over it (shared
  session across turns). 3 unit tests + a `roundtrip` test (real socket) + an
  end-to-end smoke against a live `jan-klod serve`. `make chat`. No core changes.
- [x] **TUI** — `jan-klod-ui tui [addr] [session]` layers `ratatui` (0.29, bundled
  crossterm) over `send_turn`: a scrollable transcript + an input box. The state
  lives in a pure `app::App` model (typing/backspace/submit/record), unit-tested (4
  tests); the render/event loop is thin terminal-bound glue (not auto-tested — needs
  a TTY). GUI (`--gui`, Tauri) stays deferred.

**Exit gate:** ✓ (request→response). The client drives a running `jan-klod serve` over
REST and holds a multi-turn shared-session conversation (line REPL or TUI); verified
by the `roundtrip` test, the `App`-model tests, and a live e2e smoke.

---

## Slice 4b — `host-socket` + `chat-telegram`

- [x] **Transport need resolved: no `host-socket` for v1.** The Telegram Bot API is
  **outbound HTTP only** — `getUpdates` (long-poll GET) + `sendMessage` (POST) — so
  the existing outbound HTTP covers it. `wit/host-socket.wit` is **not built** (YAGNI;
  revisit for a chat platform that needs a persistent inbound socket, e.g. a
  websocket-only API).
- [x] **Build `chat-telegram`** — `jan_klod_core::telegram`: `parse_updates` /
  `next_offset` (pure, tested) + `poll_once(agent, fetch, token, offset)` that fetches
  updates, drives each message through the loop (chat id = durable session), and
  sends the answer back. Host-side (drives the host loop, consistent with the REST
  surface); HTTP injected as a `Fetch` closure for offline testing. Launchable:
  `jan-klod telegram` (`make chat-telegram`, `TELEGRAM_BOT_TOKEN`).

**Exit gate:** ✓ `host/tests/telegram.rs` — a canned inbound message drives one
`poll_once` cycle through the sandboxed guests and the captured `sendMessage` carries
the answer to the right chat, offline, no UI client. Wired into `make harness`.

---

## Slice 4c — `agent-*` ACP delegation

- [x] **Outbound** — `jan_klod_core::delegate`: delegation is something the loop
  *calls*, so `AgentDelegate` plugs into the conductor's existing `ToolInvoker` seam.
  The model emits a `delegate` tool call (`{agent, task, context?}`); `AgentDelegate`
  resolves the agent to its ACP endpoint and forwards the task over an injected
  `AgentTransport` (the ACP wire lives behind the trait), returning the remote
  answer as the tool result. `tool_definition()` advertises the tool. 5 unit tests
  (delegate → endpoint; ignore non-delegate; unknown agent; transport failure; tool
  def). *(A concrete ACP-over-HTTP `AgentTransport` + wiring `AgentDelegate` into
  `build_agent` from `agent.*` config is the remaining glue — the seam + ReAct tool
  loop that carries it are both tested.)*
- [x] **Inbound** — no new code: core is callable by an ACP orchestrator over the
  host-side REST surface (`serve`) — the orchestrator `POST`s a task as a turn and
  reads the answer. The concrete ACP↔REST framing is a thin adapter over that surface.

**Exit gate:** ✓ a delegated task round-trips offline — `AgentDelegate.invoke` forwards
to a stub transport and returns its answer; the loop's ReAct tool cycle that carries
tool results is separately gated (`phase2_gate`).

---

## Slice 4d — Exit gate

- [x] Integration tests: the UI client drives core over REST (`jan-klod-ui`
  `roundtrip`, 4a) and an inbound Telegram message drives a turn + reply
  (`host/tests/telegram.rs`, 4b), both offline.
- [x] CI — `make phase4-gate` runs both, wired into the harness job in
  `.github/workflows/ci.yml`.
- [x] **Mark Phase 4 `done`** here and in [roadmap.md](../../concepts/roadmap.md).
  Carried-forward, non-blocking: `ratatui` GUI (`--gui`, Tauri), the concrete
  ACP-over-HTTP `AgentTransport` + wiring `AgentDelegate` into `build_agent`, and
  `host-socket` (only if a future chat platform needs a persistent inbound socket).

**Definition of done:** `make phase4-gate` passes in CI (green); `roadmap.md` status
tracker updated to `done`. ✓

## Cross-cutting (continuous)

- Strict clippy on every new crate; `cargo test` green per PR; supply-chain gates
  extended to any new dep (e.g. `ratatui`/`crossterm`).
- Client ↔ core stays on the recorded REST contract; no bespoke transport.
