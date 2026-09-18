---
type: decision
title: MVP WASM Host — ABI, no-CGo, SQLite placement (superseded)
description: Wazero has no Component Model → core-module JSON ABI; no CGo anywhere; SQLite runs host-side not in-guest. Superseded by 2026-06-29.
tags: [decision, wasm, wazero, abi, sqlite, superseded]
created: 2026-06-28
updated: 2026-06-28
---

# Handoff — MVP WASM Host: ABI, no-CGo, SQLite placement

> **Superseded** by [2026-06-29 — Component Model on Rust + Wasmtime](../2026-06-29-component-model-rust/Handoff.md).
> The core-module JSON ABI below exists because Wazero had no Component Model;
> the host is Wasmtime now and extensions are components with WIT contracts, so
> the ABI reasoning is history. **What outlived it:** SQLite runs host-side, not
> in a guest — still true, and re-argued from scratch in
> [2026-08-10 — Storage is not an extension](../2026-08-10-storage-is-not-an-extension/Handoff.md).

## What this is

First MVP slice: Go core loads sandboxed WASM, calls across boundary. Three findings revise (not overturn) [Go + Wazero stack](../2026-06-28-go-wasm-stack/Handoff.md).

Architecture (model, taxonomy, loop, deployment) unchanged. Only: runtime mechanism and SQLite placement.

---

## Findings

### 1. Wazero: no Component Model

**wazero runs core WASM only** — no component/Canonical ABI. Mature Go host is `wasmtime-go` (CGo; violates no-CGo).

**Resolution:** MVP uses **JSON-over-linear-memory ABI**. WIT stays canonical; ABI is encoding. Swappable when wazero supports components.

### 2. No CGo

- Core/guests: no CGo
- SQLite: `modernc/sqlite` or `ncruces/go-sqlite3` (no CGo)
- UI-GUI (Wails): optional, not MVP

Build: `CGO_ENABLED=0` → static binaries, easy cross-compile.

### 3. SQLite on host, not guest

Stack planned `store-sqlite.wasm` inside guest; `modernc/sqlite` doesn't target wasip1.

**Resolution:** SQLite is **host-side capability** exposed via `host-storage` contract (not `.wasm` guest). MVP validates with `store-memory.wasm` (proves roundtrip). Library choice deferred.

---

## The ABI (MVP)

Core WASM module. Requests/responses are JSON through guest linear memory.

Guest exports:

| Export | Signature | Purpose |
|---|---|---|
| `alloc` | `(size u32) -> ptr u32` | Host asks guest to reserve `size` bytes |
| `free` | `(ptr u32)` | Host releases a guest buffer |
| `invoke` | `(ptr u32, len u32) -> u64` | Process request; returns `resultPtr<<32 \| resultLen` |

Host module `jan-klod` (imported by the guest):

| Import | Signature | Maps to |
|---|---|---|
| `log` | `(level u32, ptr u32, len u32)` | `wit/host-log.wit` |
| `config_get` | `(keyPtr u32, keyLen u32) -> u64` | `wit/host-config.wit` |
| `http_fetch` | `(reqPtr u32, reqLen u32) -> u64` | `wit/host-http.wit` |

Results written to caller's linear memory via guest's `alloc`, return `Pack(ptr, len)`, guest reads & `free`s. `config_get` & `http_fetch` use JSON; HTTP bodies base64. `config_get` serves per-extension config.

**Deviation:** `http_fetch` returns `ok=true` + status/body for completed exchanges (includes API errors), `ok=false` only for transport failures.

Note: Go `wasip1` is reactor (`_initialize`); wazero needs `WithStartFunctions("_initialize")`.

---

## Code map (this slice)

All Go source lives under `src/` (module root). Build artifacts go to `bin/`
and `ext/` at the repo root.

| Path | Role |
|---|---|
| `src/cmd/jan-klod/` | Entry point; boots host, loads configured extensions, runs roundtrip smoke test |
| `src/internal/config/` | parses `jan-klod.yaml` (per-extension sections, env expansion) |
| `src/internal/host/runtime.go` | wazero runtime + WASI + `jan-klod` host module registration |
| `src/internal/host/hostfuncs.go` | host-config + host-http implementations + guest-return helper |
| `src/internal/host/loader.go` | config-driven load + lifecycle + extension registry |
| `src/internal/host/extension.go` | load / instantiate / call / lifecycle of an extension |
| `src/internal/abi/abi.go` | pointer+length packing convention |
| `src/extensions/store-memory/` | in-memory `memory-store` guest (separate module) |
| `src/extensions/provider-openai/` | OpenAI-compatible `llm-provider` guest (non-streaming) |
| `src/extensions/probe-host/` | test-fixture guest exercising host-log/config/http |
| `jan-klod.yaml` | runtime config: which extensions load + their settings |
| `Makefile` | `make all` builds guests `.wasm` + host; `make run`; `make test` |

---

## Open questions (carried forward)

- Retry limit before surfacing an error to the user (tentative: 3).
- Context compression strategy in `manager-context` (deferred).
- ACP delegation timeout (tentative: 30s).
- Persistent store library: `modernc/sqlite` vs `ncruces/go-sqlite3` (deferred
  to when the host-side store is built).
- Whether to revisit the component model once wazero supports it.
