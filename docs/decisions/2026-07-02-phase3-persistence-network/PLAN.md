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
| 3a — Host-side persistent store (`store-sqlite`) | `not-started` |
| 3b — `host-serve` capability + `api-rest` (REST + SSE) | `not-started` |
| 3c — UI ↔ core transport resolution | `not-started` |
| 3d — Exit gate | `not-started` |

---

## Slice 3a — Host-side persistent store

Make state durable behind the existing storage contracts, so the loop's history
(and any interceptor storage) survives a restart.

- [ ] **Decide the store boundary** — host proxies `host-storage` to a SQLite DB
  directly, vs. a thin `store-sqlite` guest that routes to a host DB capability.
  Record as `decisions/2026-…-host-side-sqlite/`. (Lean: host proxies directly —
  the sandbox can't hold the DB anyway, and the in-memory `host-storage` impl in
  `interceptor_host` already shows the host-side shape.)
- [ ] **Add the SQLite backend in core** (`rusqlite` bundled): a `Store` with the
  `memory-store`/`host-storage` operations (`set`/`get`/`delete`/`list-keys`/
  `recent`, plus `search`/`purge-namespace` for the fuller `memory-store`), backed
  by a `namespace/key/value/created-at/updated-at` table; `path` from config.
- [ ] **Wire it as the host-storage backend** the core serves to extensions
  (replacing the per-adapter in-memory maps), and persist the loop's session
  history through it.
- [ ] **Supply-chain** — `rusqlite` (+ bundled SQLite C) passes `cargo-deny`/`cargo-audit`;
  record any license/advisory notes.

**Exit gate:** an integration test writes state, drops and re-opens the `Runtime`
against the same DB file, and reads the state back (survives restart).

---

## Slice 3b — `host-serve` + `api-rest`

Give the outside world a way in.

- [ ] **Design `wit/host-serve.wit`** — a host-owned inbound listener the `api-*`
  guest binds (`serve(bind-addr)`), with the host calling the guest back per
  request (handler registration), returning a response. Keep the socket in the host.
- [ ] **Promote the driver/loop-entry WIT** — the Phase 2 core-Rust entry
  (`run` / `next-event` / `provide-answer` / cancel + steering) becomes the shape
  `api-rest` imports to drive turns.
- [ ] **Build `api-rest`** (REST + SSE): a `POST /turns` (or `/chat`) that drives
  the loop and streams `next-event` over SSE; host side on `axum`/`tokio`.
- [ ] **Resolve the HTTP-framework + async plumbing** (`axum`) and record it.

**Exit gate:** an external HTTP client `POST`s a query and receives the streamed
answer over SSE, driving the real loop end-to-end.

---

## Slice 3c — UI ↔ core transport

- [ ] **Confirm the transport** — UI clients connect via `api-rest` (HTTP+SSE);
  record the decision (does a UI deployment always require `api-rest`? current
  lean: yes). No core code beyond what 3b provides; this is a recorded decision +
  any client-contract notes that unblock Phase 4's `jan-klod-ui`.

**Exit gate:** the transport decision is recorded and consistent with `api-rest`.

---

## Slice 3d — Exit gate

- [ ] Integration test(s): **state persists across a `Runtime` restart** (3a) **and
  an external HTTP client drives core via `api-rest`** (3b), both offline/local.
- [ ] CI — a `make phase3-gate` added to `.github/workflows/ci.yml`.
- [ ] **Mark Phase 3 `done`** here and in [roadmap.md](../../concepts/roadmap.md);
  begin Phase 4 planning.

**Definition of done:** `make phase3-gate` passes in CI; `roadmap.md` status tracker
updated to `done`.

## Cross-cutting (continuous)

- Strict clippy (`[workspace.lints]`) on every new crate/module — same policy as
  Phases 1–2.
- `cargo test` stays green on every PR; supply-chain gates extended to any new
  dependency (notably `rusqlite`, `axum`, `tokio`).
- Structured logging tags every new component/host capability with its name.
