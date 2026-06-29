---
type: decision
title: MVP WASM Host — ABI, no-CGo, SQLite placement (superseded)
description: Wazero has no Component Model → core-module JSON ABI; no CGo anywhere; SQLite runs host-side not in-guest. Superseded by 2026-06-29.
tags: [decision, wasm, wazero, abi, sqlite, superseded]
created: 2026-06-28
updated: 2026-06-28
---

# Handoff — MVP WASM Host: ABI, no-CGo, SQLite placement

## What this is

The first implementation slice of the MVP: a Go core that loads a sandboxed
WASM extension and calls it across the contract boundary. Building it surfaced
three findings that revise — but do not overturn — the
[Go + Wazero + WASM stack decision](../2026-06-28-go-wasm-stack/Handoff.md).

The architectural concepts (extension model, taxonomy, agent loop, deployment)
all carry forward. What changed is the concrete runtime mechanism and where
SQLite runs.

---

## Findings

### 1. Wazero does not support the WASM Component Model

The stack doc says extensions are *"WASM component model + WIT"*. In practice
**wazero runs only core WASM modules** — it has no component-model / Canonical
ABI support. The only mature Go host that runs components is `wasmtime-go`,
which requires **CGo** and so violates the locked pure-Go / no-CGo decision.

**Resolution.** For the MVP, extensions are **core WASM modules with a
hand-rolled JSON-over-linear-memory ABI**. The WIT files remain the canonical
contract spec; the ABI is their concrete encoding. If/when wazero gains
component support (or the component toolchain matures for Go guests), the ABI
layer can be swapped without changing the WIT contracts or extension logic.

### 2. No CGo anywhere in the MVP

Confirmed end to end:

| Component | CGo |
|---|---|
| Core host (wazero) | No |
| Guest extensions (`GOOS=wasip1 GOARCH=wasm`) | No |
| Host-side SQLite (when added) | No (`modernc/sqlite` or `ncruces/go-sqlite3`) |
| `ui-gui` (Wails WebView) | Yes — **optional, not in MVP** |

Build with `CGO_ENABLED=0`. This is a feature: static binaries, trivial
cross-compilation, no C toolchain.

### 3. SQLite runs on the host, not inside the wasm guest

The stack doc plans `store-sqlite.wasm` using `modernc/sqlite` *inside* the
guest. That does not work: `modernc/sqlite` does not target the `wasip1` guest.

We are **not locked to a specific SQLite library — only to using SQLite** as the
engine. Two CGo-free options both run on the **host** side:

| Library | Mechanism |
|---|---|
| `modernc/sqlite` | SQLite C transpiled to native Go |
| `ncruces/go-sqlite3` | SQLite compiled to wasm, run on wazero; `database/sql` driver |

**Resolution.** A persistent SQLite store is a **host-side capability** the core
exposes to extensions through the `memory-store` / `host-storage` contract — not
a `.wasm` guest. The MVP validates the contract first with an in-memory
`store-memory.wasm`, which proves the host<->guest roundtrip without depending on
SQLite-in-wasm. Library choice between the two options is deferred to when the
persistent store is actually built.

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

Host functions that return data write the result into the **caller's** linear
memory via the guest's own `alloc` export and return `Pack(ptr, len)`; the guest
reads the bytes and `free`s the buffer. `config_get` and `http_fetch` exchange
JSON envelopes; HTTP request/response bodies are base64-encoded. `config_get`
identifies the calling extension by `m.Name()` and serves only that extension's
config section.

**ABI deviation from WIT (http-error).** `wit/host-http.wit` models 4xx/5xx as
error variants. The ABI instead returns `ok=true` with `status`+`body` for any
completed exchange (so callers can read API error payloads) and `ok=false` only
for transport failures (`invalid-url`, `connection-failed`, `timeout`).

Remaining host interfaces (`host-event`, `host-storage`) follow the same pattern
as extensions are built.

Key implementation note: Go's `wasip1` `-buildmode=c-shared` output is a
**reactor** (`_initialize`, not `_start`), so wazero must be configured with
`WithStartFunctions("_initialize")`.

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
