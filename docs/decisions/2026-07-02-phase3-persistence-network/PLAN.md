# Phase 3 Plan — Persistence + Inbound Network

Living execution checklist for Phase 3 of the [Roadmap](../../concepts/roadmap.md).
Update the flags here and in the roadmap [Status tracker](../../concepts/roadmap.md#status-tracker)
as work proceeds.

**Prerequisite:** Phase 2 exit gate passed (2026-07-02) — the thin loop runs
end-to-end, but all state is in-memory and nothing outside the process can reach
core.

Relevant background: [architecture.md → Storage](../../concepts/architecture.md#storage) ·
[Transport](../../concepts/architecture.md#transport) ·
[contracts.md](../../concepts/contracts.md) ·
[`wit/host-storage.wit`](../../../wit/host-storage.wit) ·
[`wit/memory-store.wit`](../../../wit/memory-store.wit).

## Goal

Durable state that survives a restart, and a way for the outside world to drive
core. Two independent capabilities land: a **host-side persistent store** behind
the existing `host-storage`/`memory-store` contracts, and a **`host-serve`**
inbound-listener capability with a first `api-rest` (REST + SSE) surface that
drives the Phase 2 loop entry.

## Architecture invariants carried in

- **Persistence is host-side.** The sandbox grants no filesystem, so the SQLite
  database lives in **core** (a host capability), *not* SQLite-in-wasm. A
  `store-*` extension either stays a thin guest that routes to the host store, or
  is subsumed by the host proxying `host-storage` directly — resolved in Slice 3a.
  See [architecture.md:289](../../concepts/architecture.md#storage).
- **UIs are separate client processes**, not extensions; they reach core over an
  `api-*` HTTP+SSE surface (the LSP/server model). Current lean: a UI deployment
  always includes `api-rest`.
- **The loop entry already exists** (`Runtime::build_agent` → `AgentSession::run_with`,
  Phase 2). `api-rest` is a *driver* of that entry; the driver capability WIT shape
  is promoted from the Phase 2 core-Rust entry when inbound network lands here.

## TBD resolutions (recommended leans — confirmed at the slice that needs each)

Per YAGNI, each is closed at its slice and recorded as a dated decision under
`decisions/`. Leans below, from [architecture.md](../../concepts/architecture.md):

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| Host-side SQLite library | Slice 3a | `rusqlite` with the **bundled** feature | Host-side (no sandbox/wasm constraint), mature, embeds SQLite so there is no system-lib dependency; synchronous fits the current sync Wasmtime host. |
| SQL layer | Slice 3a | **`rusqlite` directly** (no `sqlx`) | The store is a small KV+history schema (namespace/key/value/timestamps); `sqlx`'s async + compile-time checking is weight this schema does not need. Revisit if `store-postgres` (Phase 3+) wants one driver abstraction. |
| `host-serve` capability shape | Slice 3b | A host-owned listener: `serve(bind) -> listener`; the extension registers request handlers the host calls back (mirrors how `host-http` inverts for outbound). | Keeps the socket in the host (sandbox has no network-listen); the `api-*` guest stays pure request→response. |
| HTTP framework (host side) | Slice 3b | `axum` (on `tokio`, already the async runtime chosen for `host-http`) | Mature, `tower` ecosystem, first-class SSE; reuses the Phase 1 `tokio` decision. |
| UI ↔ core transport | Slice 3c | UI client always connects via `api-rest` (HTTP + SSE) | Matches the LSP/server model already in architecture.md; no bespoke transport. |

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Slice | Flag |
|---|---|
| 3a — Host-side persistent store (`store-sqlite`) | `done` |
| 3b — `host-serve` capability + `api-rest` (REST + SSE) | `done` |
| 3c — UI ↔ core transport resolution | `done` |
| 3d — Exit gate | `done` |

---

## Slice 3a — Host-side persistent store

Make state durable behind the existing storage contracts, so the loop's history
(and any interceptor storage) survives a restart.

- [x] **Store boundary decided: host proxies directly.** The core owns the
  `Store` and serves it host-side; there is no `store-sqlite` *guest* (the sandbox
  can't hold the DB). `Runtime::open_store` selects the backend from the enabled
  `store.*` instance (`store.sqlite` + `path` → durable file; else in-memory).
- [x] **Add the SQLite backend in core** — `jan_klod_core::store::Store` (`rusqlite`
  bundled): the full `memory-store` op set (`set`/`get`/`delete`/`list-keys`/`recent`/
  `search`/`purge-namespace`) over a `namespace/key/value/created-at/updated-at`
  table, upsert preserving `created-at`, `recent`/`list-keys` newest-first (rowid
  tiebreak for same-second writes). `open(path)`/`open_in_memory()`. 7 tests incl.
  **state survives a reopen**. Bundled SQLite builds natively (no rustup needed).
- [x] **Give the store a real consumer** — `AgentSession` persists each completed
  turn's `{user, answer}` to the session's durable transcript (`Store::set` under
  `namespace = session`); `AgentSession::transcript(session)` reads it back. *(Backing
  the interceptor `host-storage` import with the shared `Store` — replacing the
  in-memory map in `interceptor_host` — is deferred until a guest actually writes
  through it; the transcript path already exercises the wired host-side store.)*
- [x] **Supply-chain** — the new deps (`rusqlite`/`libsqlite3-sys`/`hashlink`/
  `fallible-iterator`/`fallible-streaming-iterator`) are all MIT / MIT-OR-Apache-2.0,
  covered by the `deny.toml` allow-list; bundled SQLite C is public-domain. Passes
  `cargo-deny` (run in CI).

**Exit gate:** ✓ `host/tests/persistence.rs` — a turn's transcript is written, the
whole `Runtime` (and its SQLite connection) is dropped, a fresh `Runtime` boots
against the same DB file, and the transcript reads back intact. Wired into
`make harness`.

---

## Slice 3b — `host-serve` + `api-rest`

Give the outside world a way in.

- [x] **Boundary decided: the REST surface is host-side, not a wasm guest.** The
  loop it drives is host mechanism, and the wasip2 sandbox grants no inbound
  sockets, so `host-serve`-as-guest-callback would add a WIT/plumbing layer with no
  isolation benefit for trusted infrastructure code. A `wit/host-serve.wit` +
  `api-rest` *guest* stays available for domain-specific surfaces later; v1 is
  `jan_klod_core::serve`. *(Supersedes the guest/`host-serve` framing in this
  slice's original checklist.)*
- [x] **HTTP framework decided: `tiny_http` (synchronous), not `axum`/`tokio`.** The
  loop is sync and `AgentSession` is `!Send` (Wasmtime + `rusqlite`), so a blocking
  server that serves one turn at a time on the session's own thread is the right fit
  — no async runtime, no cross-thread session sharing. (`axum` returns if/when an
  async, multi-session surface is warranted.)
- [x] **Build `api-rest` (request→response)** — `serve::handle_turn` (`{session?,
  message}` → `{answer, agentic}`), `serve::serve_once` / `serve::serve` over
  `tiny_http`. Pure `handle_turn` is unit-testable; `serve_once` drives one HTTP
  round-trip.
- [x] **Launchable surface** — `jan-klod serve [config] [ext] [bind]` boots the agent
  with live `host-http` and serves turns (`make serve`, default `127.0.0.1:8787`).
  Smoke-tested: a `POST` drives the loop and returns JSON.
- [ ] **SSE streaming** — stream `next-event` over Server-Sent Events. Deferred with
  the streaming run-handle (Phase 2 carry-forward); v1 returns the whole answer.

**Exit gate:** ✓ `host/tests/api_rest.rs` — an external HTTP client (on a separate
thread) `POST`s a query to the bound port and reads back `200 OK` with the answer,
driving the real loop through the sandboxed guests, offline. Wired into `make harness`.

---

## Slice 3c — UI ↔ core transport

- [x] **Transport confirmed: UI clients connect via the host-side REST surface
  (HTTP; SSE once streaming lands).** A UI deployment **always includes** the REST
  surface — the LSP/server model: core is the server, the UI a thin client. In v1
  this is `jan_klod_core::serve` (host-side), not a separate `api-rest` guest (see
  Slice 3b). Client contract for Phase 4's `jan-klod-ui`: `POST` a JSON turn
  (`{session?, message}`) → `{answer, agentic}`; streaming (SSE) arrives with the
  run-handle. No new core code beyond 3b.

**Exit gate:** ✓ the transport decision is recorded and consistent with the 3b REST
surface.

---

## Slice 3d — Exit gate

- [x] Integration tests: **state persists across a `Runtime` restart** (`persistence.rs`,
  3a) **and an external HTTP client drives the loop over the REST surface**
  (`api_rest.rs`, 3b), both offline/local.
- [x] CI — `make phase3-gate` runs both, wired into the harness job in
  `.github/workflows/ci.yml`.
- [x] **Mark Phase 3 `done`** here and in [roadmap.md](../../concepts/roadmap.md).
  Carried-forward, non-blocking refinements: SSE streaming (with the run-handle),
  and backing the interceptor `host-storage` import with the shared `Store`.

**Definition of done:** `make phase3-gate` passes in CI (green); `roadmap.md` status
tracker updated to `done`. ✓

## Cross-cutting (continuous)

- Strict clippy (`[workspace.lints]`) on every new crate/module — same policy as
  Phases 1–2.
- `cargo test` stays green on every PR; supply-chain gates extended to any new
  dependency (notably `rusqlite`, `axum`, `tokio`).
- Structured logging tags every new component/host capability with its name.
