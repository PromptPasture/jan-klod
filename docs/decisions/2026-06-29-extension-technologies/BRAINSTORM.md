---
topic: Extension Technologies
method: comparative analysis
date: "2026-06-29"
related:
  - ../2026-06-29-component-model-rust/Handoff.md
  - ../../concepts/roadmap.md
  - ../../concepts/architecture.md
  - ../../concepts/contracts.md
---

# Brainstorm - Extension Technologies

## Goal

Decide which languages/toolchains we use for our **own first-party ("built")
extensions** — `provider-*`, `store-*`, `manager-*`, `registry-*`, `tool-*`,
`api-*`, `chat-*`. The architecture is already polyglot (any `wit-bindgen`
language can author a component); this brainstorm narrows *our* picks, not the
ecosystem's freedom.

## Context

- `core` is Rust-only; extensions are sandboxed WASM components against the
  canonical `wit/` contracts (see
  [Component Model on Rust + Wasmtime](../2026-06-29-component-model-rust/Handoff.md)).
- **Polyglot in principle ≠ mature CM toolchain today.** Our own history is the
  warning: `wit-bindgen-go` immaturity in the Go/wazero MVP forced the poll-based
  streaming workaround (see [Contracts](../../concepts/contracts.md#streaming)).
- The project premise is **"nothing is trusted"**, which the user extended to the
  *build pipeline*: recent npm registry worms (token-stealing, self-republishing
  packages) make supply-chain posture a first-class selection criterion.

## Agenda

1. Strategy: one default language vs per-extension best-fit.
2. The maturity-vs-ecosystem tension (what wins when they disagree).
3. Supply-chain / security posture as a selection criterion.
4. The approved language menu + tentative near-term assignments + tracking.

## Ideas Considered

### Per-extension best-fit, maturity- and security-gated (chosen)

- **Description:** Choose language per extension, but gate the choice by (1) CM-guest
  toolchain maturity, (2) supply-chain posture, then (3) ecosystem fit, with
  case-by-case overrides.
- **Benefits:** Best tool per job; honest about toolchain reality; keeps the build
  pipeline off the worst attack surfaces; still genuinely polyglot.
- **Trade-offs:** More than one toolchain to maintain/CI; needs a written rule so
  "best-fit" doesn't drift into "whatever's trendy."

### Single default language for everything

- **Description:** Pick one language for ~all our extensions.
- **Benefits:** One toolchain, consistent codebase, simplest CI.
- **Trade-offs:** Throws away the Component Model's core payoff; forces a poor fit on
  integration extensions whose best library lives elsewhere.

### Ecosystem-first (absorb toolchain pain)

- **Description:** Always pick the best-SDK language even if its CM path is immature.
- **Benefits:** Maximum per-extension ecosystem fit.
- **Trade-offs:** Repeats the Go-streaming-workaround experience; couples us to
  forks/pre-release toolchains; ignores supply-chain risk.

## Outcomes

### Summary

We adopt **per-extension best-fit, gated**. The build pipeline is *not* protected by
our runtime WASM sandbox (npm-style worms fire at install/build time on dev/CI
machines), so supply-chain posture weighs alongside toolchain maturity. That pushes
the default to **Go (TinyGo) or Rust** — both CM-mature with strong supply-chain
posture — and demotes large/scripted package ecosystems to case-by-case use.

### Decisions

1. **Selection rule (priority order):** (A) CM-guest toolchain maturity gates the
   choice → (2) supply-chain/security posture → (3) ecosystem fit, with (C)
   case-by-case overrides when an ecosystem's library is *decisive*.
2. **Default menu: Go (TinyGo) or Rust.** Pick per extension — **Rust** where
   perf/safety or a crate is decisive; **Go** for orchestration, leanest supply
   chain, team familiarity.
3. **Case-by-case only: TypeScript/JS and Python** — allowed when a library in that
   ecosystem is decisive *and* hygiene controls are applied.
4. **Excluded for now: Kotlin/JVM, Java** — Kotlin/Wasm is Beta, needs a
   `wit-bindgen` fork, WASI threading unresolved; Java has no official WASM target.
   Revisit when those stabilize.
5. **Cross-cutting supply-chain controls (all languages):** pinned lockfiles +
   integrity hashes, minimal vetted deps, disabled install scripts
   (`--ignore-scripts` / no surprise `build.rs`), vendored/mirrored registry,
   SBOM + dependency audit in CI, prefer signed/provenance artifacts.
6. **Threat-model note:** the WASM sandbox confines the shipped extension at runtime;
   it does **not** protect the build/CI machine or artifact integrity. Supply-chain
   risk is managed at build time, which is why it gates language choice.

### Language menu (CM-guest maturity, early 2026)

| Tier | Language | CM-guest path | Use for our extensions |
|---|---|---|---|
| First-class | Rust | `cargo-component` + `wit-bindgen` | `core`; extensions where perf/safety/crate is decisive |
| Default (non-Rust) | Go | **TinyGo** v0.34+ (native CM) + `wit-bindgen-go` + `wkg` | Default for our extensions; use TinyGo, not std Go (GC caveat) |
| Case-by-case | TypeScript/JS | `jco` / ComponentizeJS | Only when a JS library is decisive + hygiene |
| Case-by-case | Python | `componentize-py` | Only when a Python library is decisive + hygiene |
| Excluded (for now) | Kotlin/JVM, Java | Beta / `wit-bindgen` fork / no Java target | Revisit when Kotlin/Wasm + WASI threading stabilize |

### Near-term assignments (provisional — confirmed at the Phase 1 gate)

| Component | Phase | Language | Status |
|---|---|---|---|
| `core` | — | Rust | not-started |
| slice-1a stub `provider` | 1 | Go (TinyGo) | not-started |
| `store-memory` | 1 | Go (TinyGo) | not-started |
| `provider-openai` | 1 | Go (TinyGo) | not-started |
| `manager-agent-loop` | 2 | Go (TinyGo) | not-started |
| `manager-context` | 2 | Go (TinyGo) | not-started |
| `chat-telegram`, library-heavy tools | later | TS/Python case-by-case + hygiene | not-started |

Near-term footprint: **Rust (core) + Go (extensions)** — polyglot, off the npm
attack surface. Status flags here mirror the phase tracker in
[Roadmap](../../concepts/roadmap.md#status-tracker).

### Open Questions

- Confirm the TinyGo CM path end-to-end at the Phase 1 gate (`wkg` dependency
  resolution, the `wasi:cli` world assumption; GC caveat avoided by using TinyGo).
- Define the concrete CI hygiene checklist (SBOM tool, per-ecosystem audit tooling).
- Revisit Kotlin/JVM once Kotlin/Wasm leaves Beta and WASI threading lands.

## Next Steps

1. Record the language policy as a ground rule in [Roadmap](../../concepts/roadmap.md)
   and add a phase status tracker so phases run as a resumable loop.
2. At the Phase 1 gate, stand up the TinyGo component toolchain + the supply-chain
   CI controls.
3. Promote this brainstorm's decision into the phase work as each extension is built.
