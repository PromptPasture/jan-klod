# Phase 3 Plan — Persistence + Inbound Network

Execution checklist for Phase 3. Update flags here and in [Status tracker](../../concepts/roadmap.md#status-tracker).

**Prerequisite:** Phase 2 exit gate (2026-07-02) — thin loop end-to-end, all in-memory, closed process.

References: [architecture.md → Storage](../../concepts/architecture.md#storage) · [Transport](../../concepts/architecture.md#transport) · [`host-storage.wit`](../../../wit/host-storage.wit).

## Goal

Durable state surviving restarts + way for outside to drive core. Two capabilities: 
**host-side persistent store** behind `host-storage`/`memory-store` contracts, and 
**`host-serve`** inbound listener with **`api-rest`** (REST + SSE) surface driving Phase 2 loop.

## Architecture invariants

- **Persistence host-side.** Sandbox has no filesystem; SQLite lives in **core** (host capability), not wasm. `store-*` extension routes to host store or subsumed. [architecture.md:289](../../concepts/architecture.md#storage).
- **UIs are separate clients**, not extensions; reach core via `api-*` HTTP+SSE (LSP/server model). UI deployments always include `api-rest`.
- **Loop entry exists** (`AgentSession::run_with`, Phase 2). `api-rest` drives it; driver WIT shape promoted from core-Rust entry.

## TBD resolutions

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| Host-side SQLite | Slice 3a | `rusqlite` bundled | Host-side (no sandbox), mature, embeds SQLite, sync fits. |
| SQL layer | Slice 3a | `rusqlite` directly (no `sqlx`) | Small KV+history schema; `sqlx` async/compile-time checking unneeded. |
| `host-serve` shape | Slice 3b | Host-owned listener; extension registers handlers (inverted from `host-http`). | Socket stays in host (sandbox no network-listen); `api-*` guest pure request→response. |
| HTTP framework | Slice 3b | `tiny_http` (sync) not `axum` | Loop sync, `AgentSession` !Send; blocking server on session thread. |
| UI ↔ core | Slice 3c | HTTP + SSE via `api-rest` | LSP/server model; no bespoke transport. |

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

Make state durable behind existing storage contracts; loop history survives restart.

- [x] **Store boundary:** core owns `Store`, serves host-side. No `store-sqlite` guest. `Runtime::open_store` selects backend from `store.*` (`store.sqlite` + `path` → durable file; else in-memory).
- [x] **SQLite backend in core** — `jan_klod_core::store::Store` (rusqlite bundled): full `memory-store` ops (`set`/`get`/`delete`/`list-keys`/`recent`/`search`/`purge-namespace`) over `namespace/key/value/created-at/updated-at`, upsert preserves `created-at`, newest-first. 7 tests incl. **state survives reopen**.
- [x] **Real consumer** — `AgentSession` persists each turn's `{user, answer}` to durable transcript (`Store::set` under `namespace = session`); reads back via `AgentSession::transcript()`. *(Backing interceptor `host-storage` import deferred until guest uses it.)*
- [x] **Supply-chain** — `rusqlite` deps MIT/Apache-2.0, in `deny.toml` allow-list; bundled SQLite C public-domain. `cargo-deny` CI.

**Done:** `persistence.rs` — turn written, `Runtime` dropped, fresh `Runtime` boots same DB file, transcript reads back intact.

---

## Slice 3b — `host-serve` + `api-rest`

Give outside world a way in.

- [x] **Boundary: REST host-side, not wasm guest.** Loop drives host mechanism; wasip2 sandbox has no inbound sockets. `host-serve`-as-guest-callback adds WIT/plumbing with no isolation benefit. `api-rest` guest available later for domain-specific surfaces; v1 is `jan_klod_core::serve`.
- [x] **Framework: `tiny_http` (sync), not `axum`.** Loop sync, `AgentSession` !Send (Wasmtime + `rusqlite`); blocking server on session thread fits. No async runtime, no cross-thread sharing.
- [x] **Build `api-rest`** — `serve::handle_turn` (`{session?, message}` → `{answer, agentic}`), `serve_once`/`serve` over `tiny_http`. `handle_turn` unit-testable; `serve_once` drives one HTTP round-trip.
- [x] **Launchable** — `jan-klod serve [config] [ext] [bind]` boots agent, serves turns (`make serve`, default `127.0.0.1:8787`). `POST` drives loop, returns JSON.
- [ ] **SSE streaming** — `next-event` over Server-Sent Events. Deferred (Phase 2 carry-forward); v1 returns whole answer.

**Done:** `api_rest.rs` — external HTTP client `POST`s query, reads `200 OK` with answer, drives real loop through guests, offline.

---

## Slice 3c — UI ↔ core transport

- [x] **Transport: UI clients via host-side REST** (HTTP; SSE later). UI deployment always includes REST — LSP/server model. Client contract (Phase 4 `jan-klod-ui`): `POST` JSON turn → `{answer, agentic}`; SSE with run-handle. No new code beyond 3b.

**Done:** transport decision recorded + consistent with 3b REST surface.

---

## Slice 3d — Exit gate

- [x] Integration tests: **state persists across `Runtime` restart** (`persistence.rs`) **and external HTTP client drives loop** (`api_rest.rs`), both offline.
- [x] CI — `make phase3-gate` runs both, wired into harness job.
- [x] Mark Phase 3 `done` here and in [roadmap.md](../../concepts/roadmap.md). Carried-forward: SSE streaming, backing interceptor `host-storage`.

**Done:** `make phase3-gate` CI green; `roadmap.md` updated. ✓

## Cross-cutting

- Strict clippy (`[workspace.lints]`) on every new crate — same as Phases 1–2.
- `cargo test` green on every PR; supply-chain gates extended to new dependencies.
- Structured logging tags every new component.
