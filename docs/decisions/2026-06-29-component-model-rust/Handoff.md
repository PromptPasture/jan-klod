---
type: decision
title: Component Model on Rust + Wasmtime
description: Untrusted extensions ⇒ in-process WASM sandbox; adopt the WebAssembly Component Model; host moves Go→Rust + Wasmtime. The current foundation decision.
tags: [decision, component-model, rust, wasmtime, wit, trust-model]
created: 2026-06-29
updated: 2026-06-29
---

# Handoff — Foundation Decision: Component Model on Rust + Wasmtime

## What this is

Re-evaluated runtime foundation. [2026-06-28](../2026-06-28-mvp-wasm-host/Handoff.md) found wazero has no Component Model, forcing JSON-ABI.

**Outcome: adopt WebAssembly Component Model on Rust + Wasmtime.** Supersedes Go's runtime/host language. Architecture (taxonomy, loop, lifecycle, split, deployment, configurator) unchanged.

Prior: [2026-06-28 MVP](../2026-06-28-mvp-wasm-host/Handoff.md), [2026-06-28 Go+Wazero](../2026-06-28-go-wasm-stack/Handoff.md), [2026-06-16 Initial](../2026-06-16-jan-klod/Handoff.md)

---

## Why reopen?

Go+Wazero proved architecture end-to-end but revealed: no mature pure-Go Component Model host (forced JSON-ABI). Before building on top, we checked if the wound was load-bearing.

**Deciding question: extension trust model.**

> **Extensions are untrusted — host enforces capability-based sandbox.**

This requirement reshapes everything.

---

## Decision path

### Step 1 — Trust model rules the architecture

| Plugin model | Isolation | Safe for untrusted code? |
|---|---|---|
| Out-of-process gRPC subprocess (HashiCorp `go-plugin`, VSCode/LSP, Terraform) | OS process only | ❌ No — needs per-platform seccomp/containers bolted on |
| **In-process WASM sandbox** (Zed) | capability-based, in-process | ✅ **Yes** |

→ **Untrusted ⇒ WASM.** Confirms Zed-inspired approach. gRPC/subprocess (contracts + language-freedom) rejected: can't safely sandbox untrusted code without heavy OS work.

### Step 2 — Contract style: Component Model vs JSON-ABI

Prefer **clear contracts** + **low friction for authors**. Component Model (WIT + `wit-bindgen`) delivers typed bindings (any language); JSON-ABI requires custom encoding per author.

→ **Component Model** (ecosystem standard: Wasmtime, `wit-bindgen`, Zed).

### Step 3 — Host language forced by Steps 1+2

No mature pure-Go Component Model host.

| | Sandbox | CM | Host | CGo | Community |
|---|---|---|---|---|---|
| A. Go+wazero+JSON | ✅ | ❌ | Go | No | weak |
| B. Go+wasmtime-go+CM | ✅ | ✅ | Go | **Yes** | good |
| **C. Rust+Wasmtime+CM** | ✅ | ✅ | Rust | No | **best** |

B is a trap: CGo cost (no static binary, toolchain, cross-compile friction) without Go's benefit. Once CGo enters, Go isn't obvious.

→ **C.** Rust is the paved road for untrusted WASM: Wasmtime, `wit-bindgen`, component toolchain all Rust-native.

### Reversing Rust rejection

[2026-06-28](../2026-06-28-go-wasm-stack/Handoff.md) rejected Rust (borrow-checker hardness). Assumed **easy > correct**. User reframed:

> "Do it right and good. Bad decisions in basement collapse the tower."

Untrusted-code + capability-correctness are load-bearing; Rust ecosystem does this right. Rejection **consciously reversed** — re-weighted, not forgotten.

---

## What this changes

| Layer | Before (superseded) | After (this decision) |
|---|---|---|
| Host language | Go | **Rust** |
| Runtime | wazero (core modules) | **Wasmtime (components)** |
| ABI | hand-rolled JSON over linear memory | **Component Model / Canonical ABI** |
| Bindings | manual `alloc`/`free`/`invoke` + JSON | **`wit-bindgen` generated** |
| Guest authoring | wasip1 reactor + custom ABI | **CM component (any `wit-bindgen` language — Rust, JS, Python, Go, …)** |
| CGo | none | none (Wasmtime is Rust-native) |

## What carries forward unchanged

- **WIT contracts** — already canonical; now become the *enforced* interface,
  not just a spec. (`llm-provider`, `memory-store`, `extension-lifecycle`,
  `host-log`, `host-config`, `host-http`, etc.)
- **Polyglot extensions** — each extension is authored in whatever language
  fits it best (a provider in JS, a parser in Rust, glue in Go), all targeting
  the same WIT contracts via `wit-bindgen`. This is a *property the Component
  Model gives us for free*, not something we build — and it is the concrete
  payoff over the hand-rolled JSON-ABI, where every author had to learn a
  bespoke encoding.
- Extension taxonomy and the provider / store / manager split.
- Agent-loop design, small-model harness, blue/green deployment, configurator.
- The validated MVP behaviors (config-driven load, lifecycle, host-http,
  OpenAI-compatible provider, in-memory store) — these are *logic* and port over;
  only the host and ABI layer are replaced.

The Go work is **not wasted**: it de-risked the architecture end-to-end and
isolated the runtime mechanism as the single remaining variable — exactly the
variable this decision resolves.

---

## Runtime topology & trust

- **Nothing trusted at plugin layer.** All extensions: sandboxed WASM. No native tier. Drop "needs OS ⇒ compiled-in" conflation.
- **`core`: standalone user-process** (not daemon), headless-capable, deploy unit.
- **Agent loop is extension** (`manager-agent-loop`), not core.
- **`api-*` & `chat-*`: sandboxed WASM**, user-selected. Network via new `host-serve` (inbound) + `host-socket` (long-lived).
- **UIs: separate clients**, not extensions. Connect via `api-*` HTTP+SSE (LSP model). Single binary picks TUI/GUI. Headless: no UI.

---

## Validation spike (one day)

Test decision before porting:

1. Trivial `provider` WIT (single `complete`).
2. Rust+Wasmtime host loads component, calls across boundary.
3. **One non-Rust guest** (JS/`jco`, Python/`componentize-py`, or Go) — proves polyglot authoring.

**Exit:** CM-in-Rust clean + multi-language works → commit. Friction outweighs payoff → fallback to Go+wazero+JSON-ABI.

### TinyGo's role

TinyGo **not** community author proxy; separate supervisor/updater: stages version, blue/green flip, restart, health-check, rollback. Must survive core swap (switcher can't be switched binary). See [Blue/Green Deployment](../../concepts/blue-green-deployment.md).

---

## Open questions (carried forward / new)

- **Non-Rust guest toolchain maturity** — confirm `wit-bindgen` + the component
  toolchain (`jco` for JS, `componentize-py` for Python, `wasm-tools component
  embed/new` for Go/TinyGo) is production-ready for non-Rust authors.
- **`host-serve` / `host-socket` capabilities** — design the inbound-listener and
  long-lived-socket host interfaces that keep `api-*`/`chat-*` sandboxed; add
  them to the WIT set.
- **UI ↔ core transport** — confirm whether a UI client always connects via the
  `api-rest` extension, or whether core also exposes a small built-in local
  control endpoint so a UI works against a bare core. Current lean: require
  `api-rest`.
- Async model in the Rust host (`tokio` vs sync Wasmtime) for concurrent
  extension calls — a known Rust pain surface; scope it in the spike.
  **Resolved (2026-06-29, Slice 1a gate):** sync Wasmtime baseline; `tokio`
  enters at `host-http` (Slice 1b). See
  [Slice 1a verdict](../2026-06-29-extension-technologies/SLICE-1A-GATE.md#async-model-decision-resolves-a-phase-1-open-question).
- Host-side SQLite: prior plan used Go (`modernc/sqlite` / `ncruces/go-sqlite3`).
  Re-decide the Rust equivalent (`rusqlite` is CGo via bundled SQLite, or
  `libsql`/pure options) when the persistent store is built.
- Carry-overs still open: retry limit (~3), context compression in
  `manager-context`, ACP delegation timeout (~30s).
