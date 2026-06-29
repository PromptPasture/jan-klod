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

A re-evaluation of the extension runtime foundation, triggered by the finding
(from [2026-06-28-mvp-wasm-host](../2026-06-28-mvp-wasm-host/Handoff.md)) that
**wazero has no Component Model support**, forcing a hand-rolled JSON-over-linear-memory
ABI. We stepped back to ask whether the basement was right before building higher.

**Outcome: we are adopting the WebAssembly Component Model, hosted on
Rust + Wasmtime.** This supersedes the *runtime mechanism and host language* of
the Go + Wazero stack. The architectural concepts — extension taxonomy, agent
loop, lifecycle, provider/store split, blue/green deployment, configurator — all
carry forward unchanged. The WIT contracts remain canonical.

The prior decisions are preserved for history:
- [2026-06-28 — MVP WASM Host](../2026-06-28-mvp-wasm-host/Handoff.md) — Go + wazero + JSON-ABI (superseded by this)
- [2026-06-28 — Go + Wazero + WASM Stack](../2026-06-28-go-wasm-stack/Handoff.md) — Java→Go switch (host language now superseded)
- [2026-06-16 — Jan-Klod Initial Design](../2026-06-16-jan-klod/Handoff.md) — original Java/Quarkus vision

---

## Why we reopened the decision

The Go + Wazero work (4 working slices) proved the *architecture* end-to-end, but
exposed one wound: to talk across the sandbox boundary we hand-rolled a JSON ABI,
because **no mature pure-Go host supports the Component Model**. Before building
the agent loop on top, we tested whether that wound was load-bearing.

The deciding question turned out to be the **extension trust model**, and the
answer was explicit:

> **Extensions are untrusted — the host must enforce a hard, capability-based
> sandbox.**

That single requirement reshapes everything below.

---

## Decision path

### Step 1 — Trust model rules the architecture

| Plugin model | Isolation | Safe for untrusted code? |
|---|---|---|
| Out-of-process gRPC subprocess (HashiCorp `go-plugin`, VSCode/LSP, Terraform) | OS process only | ❌ No — needs per-platform seccomp/containers bolted on |
| **In-process WASM sandbox** (Zed) | capability-based, in-process | ✅ **Yes** |

→ **Untrusted ⇒ WASM.** This confirms the original Zed-inspired instinct. The
gRPC/subprocess "microservices-in-tree" model (attractive for contracts and
language-freedom) was rejected here *only* because it cannot safely run untrusted
code without heavy, non-portable OS sandboxing.

### Step 2 — WASM contract style: Component Model vs hand-rolled ABI

We prefer **clear, first-class contracts** and a **low-friction story for
community extension authors**. The Component Model (WIT + `wit-bindgen`) delivers
typed bindings in many guest languages with no hand-written wire glue. A
hand-rolled JSON-ABI works, but every author must learn a bespoke encoding.

→ **Component Model**, the contract style the broader ecosystem (Wasmtime,
`wit-bindgen`, Zed) is standardizing on.

### Step 3 — Host language is forced by Steps 1+2

**There is no mature pure-Go Component Model host.** That leaves three coherent
foundations:

| | Sandbox | CM contracts | Host lang | CGo | Community story |
|---|---|---|---|---|---|
| A. Go + wazero + JSON-ABI (prior MVP) | ✅ | ❌ hand-rolled | Go | No | weak — bespoke ABI |
| B. Go + `wasmtime-go` + CM | ✅ | ✅ | Go | **Yes** | good |
| **C. Rust + Wasmtime + CM** | ✅ | ✅ first-class | Rust | No | **best — `wit-bindgen`, any guest lang** |

**Option B is the trap:** it pays CGo's full cost (no static binary, C toolchain
on every build host, painful cross-compilation, a C ABI boundary) *and* gives up
the pleasant pure-Go experience — the only reason Go was chosen — without
compensating benefit. Once CGo is on the table, Go is no longer the obvious host.

→ **Option C.** For an untrusted-WASM host, Rust is the paved road, not
incidental hardness: Wasmtime, `wit-bindgen`, and the Component Model toolchain
are all Rust-native. Host and guest contract types derive from one WIT source.

### Reversing the earlier Rust rejection

The [2026-06-28 stack doc](../2026-06-28-go-wasm-stack/Handoff.md) rejected Rust
because "borrow-checker hardness [is] unacceptable when Claude Code is primary
author." That rejection assumed **easy > correct**. The user reframed the
priority explicitly:

> "It is not about simple or hard, it's about doing thing right and good. If we
> place bad decisions in the basement, the whole tower will go down."

With an **untrusted-code requirement** added, contract correctness and a sound
capability model are load-bearing, and the Rust ecosystem is where that is done
right. The earlier rejection is therefore **consciously reversed** for the host —
not forgotten, but re-weighed under a changed priority and a new requirement.

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

## Runtime topology & trust (refinement)

Re-confirmed against the original brainstorm and tightened in this session:

- **Nothing is trusted at the plugin layer.** Every extension is a sandboxed
  WASM component confined to host-granted capabilities. **There is no
  native/in-core extension tier** — the earlier "native Go/Rust extension"
  idea (compiling UI/api/chat into the binary) was a Go-era artifact and is
  dropped. It conflated *"needs OS access ⇒ not a WASM guest"* with *"⇒ compiled
  into core,"* which does not follow.
- **`core` runs as a standalone process under the user's own privileges**
  (not a system daemon), and is headless-capable. It is the deploy unit — on a
  Raspberry Pi or in a container it is the only thing you run. Untrusted plugins
  are sandboxed *inside* that user-level process.
- **The agent loop is an extension** (`manager-agent-loop`), not core — Option A
  from the original design ("zero agent behaviour in core"). Confirmed.
- **`api-*` and `chat-*` are ordinary sandboxed WASM extensions**, user-selected
  (several `api-*` exist; the user picks one or none; `chat-*` is fully
  optional). They reach the network only through new host capabilities —
  `host-serve` (inbound listener) and `host-socket` (long-lived connection) —
  since today's `host-http` is outbound-request-only.
- **UIs are not extensions.** TUI/GUI/web are optional, *separate client
  processes* that connect to core over an `api-*` HTTP+SSE surface (the LSP
  model: core is the server, the UI is a thin client). A single client binary
  selects TUI vs GUI by launch mode. A headless deployment runs no UI client.

---

## Validation before full commit (one-day spike)

To make the decision *felt*, not assumed, build a minimal spike before porting:

1. A trivial `provider` WIT interface (single `complete` function).
2. A Rust + Wasmtime host that loads a component and calls it across the CM boundary.
3. **One non-Rust guest** (language chosen for convenience — JS/`jco`,
   Python/`componentize-py`, or Go) — confirms `wit-bindgen`'s polyglot
   authoring works end to end against our host. One guest proves the mechanism;
   the breadth comes free from the ecosystem.

Exit criteria: if CM-in-Rust feels as clean as expected and the non-Rust guest
proves multi-language authoring, commit and port the slices. If the toolchain
friction outweighs the payoff, fall back to Go + wazero + JSON-ABI (Option A)
with eyes open.

### A note on TinyGo's role (not a guest-language proxy)

TinyGo is **not** the community-author proxy for the spike, and not part of the
host. Its planned role is a **small standalone supervisor/updater**: it stages a
new version, performs the blue/green flip, restarts, health-checks, and rolls
back on failure. It is deliberately a separate process from the Rust core
*because it must survive a core swap* — the thing performing the switch cannot be
the binary being switched. See
[Blue/Green Deployment](../../concepts/blue-green-deployment.md). Any guest
language used in the spike is incidental to that role.

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
- Host-side SQLite: prior plan used Go (`modernc/sqlite` / `ncruces/go-sqlite3`).
  Re-decide the Rust equivalent (`rusqlite` is CGo via bundled SQLite, or
  `libsql`/pure options) when the persistent store is built.
- Carry-overs still open: retry limit (~3), context compression in
  `manager-context`, ACP delegation timeout (~30s).
