# Phase 6 Plan — Streaming & Steering

Living execution checklist for Phase 6 of the [Roadmap](../../concepts/roadmap.md).
Update the flags here and in the roadmap [Status tracker](../../concepts/roadmap.md#status-tracker)
as work proceeds.

**Prerequisite:** Phases 1–5 done. The loop runs end-to-end but `AgentSession::run`
returns a **whole answer**; there is no incremental streaming, no mid-turn `ask`
surfaced to the driver, and no cancel/steer.

Relevant background: [architecture.md → Transport](../../concepts/architecture.md#transport) ·
[`wit/interceptor.wit`](../../../wit/interceptor.wit) (the `finalize` streaming note) ·
`jan_klod_core::conductor` · `jan_klod_core::serve`.

## Goal

The loop streams its progress incrementally, and a driver can interrupt and steer
it. Promotes the Phase 2 carry-forward — the loop entry was prototyped as a core
Rust `run_with` that returns a single `RunResult`.

## Architecture invariants carried in

- **Sync, single-threaded loop.** The conductor is synchronous and `AgentSession`
  is `!Send` (Wasmtime + `rusqlite`). So "streaming" is **push, not poll**: the
  conductor emits events to an injected sink *as it runs*, on the turn's own thread.
  The HTTP SSE handler writes each event to the response as it fires (`tiny_http`
  chunked responses). This avoids an async runtime and cross-thread session sharing.
- **Preview-vs-authoritative** (`wit/interceptor.wit` `finalize`): streamed
  `text-delta`s are a non-authoritative preview; the terminal `done` event carries
  the authoritative answer, which `after-response`/`finalize` may have rewritten.

## TBD resolutions (recommended leans — confirmed at the slice that needs each)

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| Event delivery model | 6a | An `EventSink` trait the conductor calls (`on_event(Event)`); `run_with` keeps returning the final `RunResult` for non-streaming callers | Fits the sync loop — no channels/threads; a recording sink unit-tests it; SSE is one sink. |
| Event set (v1) | 6a | `text-delta`, `tool-invoked`, `tool-result`, `warning`, `done{answer, agentic}` | Mirrors the roadmap run-handle event list; enough for a live transcript. |
| Cancel / steer (sync) | 6c | The sink returns a `Flow` (`continue`/`stop`) checked at loop boundaries; a follow-up queue drained at `prepare-next-turn` | A busy sync turn can't be pre-empted mid-token; boundary checks are the honest v1. |
| SSE framing | 6b | `event: <kind>\ndata: <json>\n\n` over a `tiny_http` streaming response | Standard SSE; curl- and browser-friendly; matches the REST surface. |

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Slice | Flag |
|---|---|
| 6a — Conductor emits events via an `EventSink` | `done` |
| 6b — SSE on the REST surface | `not-started` |
| 6c — Cancel + steering (follow-up queue) | `not-started` |
| 6d — Exit gate | `not-started` |

---

## Slice 6a — Conductor event stream

- [x] **`EventSink` + `Event`** in `conductor` (`TextDelta`/`ToolInvoked`/
  `ToolResult`/`Warning`/`Done{text, agentic}`). `run_turn` threads a sink;
  `complete_with_fallback`/`complete_validated`/`run_tool_calls` emit
  (fallback + malformed-retry warnings, tool invoked/result, per-completion delta,
  terminal `Done`). `AgentSession::run_streaming` exposes it; `run`/`run_with` pass
  `NoSink` (unchanged behaviour). Added `PartialEq`/`Eq` to `intercept::ToolCall`/
  `ToolOutcome` so `Event` compares.
- [x] **Unit tests** — a `RecordingSink` asserts the event order for a simple turn
  (`TextDelta` → `Done`), a ReAct turn (`ToolInvoked` → `ToolResult` → `TextDelta`
  → `Done`), and a fallback (leading `Warning`, terminal `Done`). 13 conductor tests.

**Exit gate:** ✓ a recording sink observes the ordered events of a multi-step turn,
ending in `Done` with the authoritative answer.

---

## Slice 6b — SSE on the REST surface

- [ ] **`POST /turn` streaming** — when the client sends `Accept: text/event-stream`
  (or `GET /turn?stream=1`), `serve` responds with a `tiny_http` streaming response
  and an SSE sink that writes `event:/data:` frames per `Event`; non-streaming clients
  keep the current single-JSON reply.
- [ ] **Clients** — `jan-klod-ui` (and the TUI) consume the SSE stream, rendering
  deltas live; Telegram may stay non-streaming (edit-message batching later).

**Exit gate:** an external HTTP client receives ordered SSE frames for a turn and a
final `done`, offline.

---

## Slice 6c — Cancel + steering

- [ ] **Cancel** — `EventSink::on_event` returns a `Flow` (`Continue`/`Stop`); the
  conductor checks it at loop boundaries (after each completion / tool result) and
  stops cleanly, emitting a `done` with what it has.
- [ ] **Steering / follow-up queue** — a queue the driver appends to; the conductor
  drains it at `prepare-next-turn`, injecting follow-up user messages into the run.

**Exit gate:** a sink that returns `Stop` after the first tool result ends the run at
the next boundary; a queued follow-up is picked up on the next iteration.

---

## Slice 6d — Exit gate

- [ ] Integration test: a driver runs a multi-step turn, receives events
  incrementally (recording sink and/or SSE), answers an `ask` mid-turn, and cancels a
  run — offline.
- [ ] CI — `make phase6-gate` added to `.github/workflows/ci.yml`.
- [ ] **Mark Phase 6 `done`** here and in [roadmap.md](../../concepts/roadmap.md);
  begin Phase 7.

**Definition of done:** `make phase6-gate` passes in CI; `roadmap.md` status tracker
updated to `done`.

## Cross-cutting (continuous)

- Strict clippy on new code; `cargo test` green per PR; the new SSE path exercised
  offline in the harness.
- Streaming stays push-based (no async runtime); the `!Send` session stays on one
  thread.
