# Phase 6 Plan — Streaming & Steering

Execution checklist for Phase 6. Update flags here and in [Status tracker](../../concepts/roadmap.md#status-tracker).

**Prerequisite:** Phases 1–5 done. Loop end-to-end but `AgentSession::run` returns whole answer; no streaming, no mid-turn `ask` to driver, no cancel/steer.

References: [architecture.md → Transport](../../concepts/architecture.md#transport) · [`interceptor.wit`](../../../wit/interceptor.wit).

## Goal

Loop streams progress incrementally; driver can interrupt + steer. Promotes Phase 2 carry-forward.

## Architecture invariants

- **Sync, single-threaded loop.** Conductor sync, `AgentSession` !Send. Streaming is **push, not poll**: conductor emits to injected sink on turn thread. HTTP SSE writes frames as they fire. No async runtime, no cross-thread sharing.
- **Preview-vs-authoritative** — streamed `text-delta`s are preview; terminal `done` carries authoritative answer, which `finalize` may rewrite.

## TBD resolutions

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| Event delivery | 6a | `EventSink` trait; `run_with` returns final `RunResult` for non-streaming | Fits sync loop; no channels/threads; sink unit-tests; SSE one sink. |
| Event set v1 | 6a | `text-delta`, `tool-invoked`, `tool-result`, `warning`, `done{answer, agentic}` | Mirrors roadmap run-handle; enough for transcript. |
| Cancel/steer sync | 6c | Sink returns `Flow` (`continue`/`stop`) checked at boundaries; follow-up queue at `prepare-next-turn` | Sync turn can't interrupt mid-token; boundaries honest v1. |
| SSE framing | 6b | `event: <kind>\ndata: <json>\n\n` over `tiny_http` streaming | Standard; curl/browser-friendly. |

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Slice | Flag |
|---|---|
| 6a — Conductor emits events via an `EventSink` | `done` |
| 6b — SSE on the REST surface | `done` |
| 6c — Cancel + steering (follow-up queue) | `done` |
| 6d — Exit gate | `done` |

---

## Slice 6a — Conductor event stream

- [x] **`EventSink` + `Event`** — `TextDelta`/`ToolInvoked`/`ToolResult`/`Warning`/`Done{text, agentic}`. `run_turn` threads sink; `complete_*`/`run_tool_calls` emit. `AgentSession::run_streaming` exposes; `run`/`run_with` pass `NoSink`. Added `PartialEq`/`Eq` to `ToolCall`/`ToolOutcome`.
- [x] **Unit tests** — `RecordingSink` asserts event order: simple turn (`TextDelta`→`Done`), ReAct (`ToolInvoked`→`ToolResult`→`TextDelta`→`Done`), fallback (`Warning`→`Done`). 13 tests.

**Done:** recording sink observes ordered events, ending `Done` with authoritative answer.

---

## Slice 6b — SSE on the REST surface

- [x] **`POST /turn` streaming** — on `Accept: text/event-stream`, `serve` gets raw socket, writes SSE headers, pushes one frame per `Event` (flushed). Non-streaming clients get single-JSON. `AgentSession::run_streaming_headless` drives; `SseSink` maps `Event`→frames (`delta`/`tool`/`tool-result`/`warning`/`done`/error).
- [x] **Client** — `jan-klod-ui` gained `stream_turn` (parses frames) + `parse_frame` (2 tests). REPL renders deltas live. End-to-end verified vs. live `serve`. TUI/Telegram non-streaming.

**Exit gate:** ✓ `api_rest.rs` — an external HTTP client sending `Accept:
text/event-stream` receives `Content-Type: text/event-stream` + ordered frames
ending in `event: done` with the answer, offline.

---

## Slice 6c — Cancel + steering

- [x] **Cancel** — `EventSink::emit` returns `Flow` (`Continue`/`Stop`); conductor checks boundaries, stops cleanly with `Done`. `SseSink` returns `Stop` on write fail — **disconnected client cancels for free**. Tested.
- [x] **Steering** — `Driver::follow_up() -> Option<String>`. Turn would end, conductor calls: `Some(msg)` dispatches `prepare-next-turn`, injects msg, cycles; `None` ends. Tested.

**Exit gate:** ✓ a sink returning `Stop` ends the run at the next boundary with a
terminal `Done`; a driver `follow_up` injects another cycle.

---

## Slice 6d — Exit gate

- [x] Tests covering the phase behaviour: the conductor event stream (delta →
  done; tool events; fallback warning), cancel (a sink `Stop`), steering (a driver
  follow-up), the SSE server (`api_rest.rs`), and the SSE client (`jan-klod-ui`
  `parse_frame` + `roundtrip`) — all offline.
- [x] CI — `make phase6-gate` aggregates them (`cargo test -- stream cancel follow_up`
  + `api_rest` + `jan-klod-ui`), wired into the harness job.
- [x] **Mark Phase 6 `done`** here and in [roadmap.md](../../concepts/roadmap.md).
  Carried-forward, non-blocking: per-token deltas (v1 streams per-completion), the
  `ratatui` TUI + Telegram consuming the stream, and the driver-capability WIT (only
  if `api-*`/`chat-*` become guests).

**Definition of done:** `make phase6-gate` passes in CI (green); `roadmap.md` status
tracker updated to `done`. ✓

## Cross-cutting (continuous)

- Strict clippy on new code; `cargo test` green per PR; the new SSE path exercised
  offline in the harness.
- Streaming stays push-based (no async runtime); the `!Send` session stays on one
  thread.
