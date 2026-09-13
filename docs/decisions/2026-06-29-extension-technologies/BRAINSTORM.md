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

Choose languages/toolchains for **first-party extensions** (`provider-*`, `store-*`, `manager-*`, `registry-*`, `tool-*`, `api-*`, `chat-*`). Architecture is polyglot; this narrows *our* picks, not ecosystem freedom.

## Context

- `core` is Rust; extensions are sandboxed WASM [components](../2026-06-29-component-model-rust/Handoff.md).
- Polyglot ≠ mature CM toolchain. Warning: `wit-bindgen-go` immaturity forced workarounds.
- "Nothing trusted" extends to build pipeline: npm worms make supply-chain first-class criterion.

## Ideas Considered

### Per-extension best-fit, gated (chosen)
Best tool per job, gated by CM maturity → supply-chain → fit. Honest about toolchain reality; genuine polyglot. Downside: multiple toolchains, drift risk without written rules.

### Single default language
One toolchain, consistent, simplest CI. Downside: wastes Component Model; poor fit on integration extensions.

### Ecosystem-first
Best-SDK language even if CM immature. Max fit. Downside: Go-streaming-workaround repeats; pre-release coupling; supply-chain ignored.

## Outcomes

### Summary

Per-extension best-fit, gated. Build pipeline unprotected (npm worms fire at install/build), so supply-chain weighs alongside maturity. Default: Go (TinyGo) or Rust (CM-mature, strong supply-chain). Large/scripted ecosystems case-by-case.

### Amendment — Default flipped to Rust

Original: Go (TinyGo) default. False premises: team/codebase are Rust-first; `cargo-component` more mature than TinyGo. **Rust now default** for first-party extensions.

**One language across core + extensions:** shared types, single CI/audit, smaller binaries, no GC. Original gates apply to non-Rust *exceptions* (library decisive).

**Polyglot guarantee unchanged.** Proof: **Slice 1a TinyGo gate** (`make gate`) — standing polyglot canary, no per-extension cost.

### Decisions

1. **Selection rule (priority order):** (A) CM-guest toolchain maturity gates the
   choice → (2) supply-chain/security posture → (3) ecosystem fit, with (C)
   case-by-case overrides when an ecosystem's library is *decisive*.
2. **Default: Rust.** First-party extensions are Rust unless another language is
   *decisive*. `core` is Rust and the team is Rust-first, so one-language
   consistency (shared types, a single CI/lockfile/audit path, no GC caveat,
   first-class `cargo-component` toolchain) outweighs per-extension ecosystem fit
   for the components *we* build. Go (TinyGo) stays a fully supported, CM-mature
   option — chosen when its ecosystem or fit is decisive — but is **no longer the
   default**. *(Amended 2026-06-29; see [Amendment](#amendment-2026-06-29--default-flipped-to-rust).)*
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
| Default | Rust | `cargo-component` + `wit-bindgen` | `core` **and all first-party extensions** |
| Case-by-case | Go | **TinyGo** v0.34+ (native CM) + `wit-bindgen-go` + `wkg` | When its ecosystem/fit is decisive; also the standing Slice 1a polyglot gate. Use TinyGo, not std Go (GC caveat) |
| Case-by-case | TypeScript/JS | `jco` / ComponentizeJS | Only when a JS library is decisive + hygiene |
| Case-by-case | Python | `componentize-py` | Only when a Python library is decisive + hygiene |
| Excluded (for now) | Kotlin/JVM, Java | Beta / `wit-bindgen` fork / no Java target | Revisit when Kotlin/Wasm + WASI threading stabilize |

### Near-term assignments (provisional — confirmed at the Phase 1 gate)

| Component | Phase | Language | Status |
|---|---|---|---|
| `core` | — | Rust | not-started |
| slice-1a stub `provider` | 1 | Go (TinyGo) | done (gate) |
| `store-memory` | 1 | Rust | not-started |
| `provider-openai` | 1 | Rust | not-started |
| `manager-agent-loop` | 2 | Rust | not-started |
| `manager-context` | 2 | Rust | not-started |
| `chat-telegram`, library-heavy tools | later | Rust, or TS/Python case-by-case + hygiene | not-started |

Near-term footprint: **Rust across `core` and all our extensions**; Go (TinyGo) is
retained only as the Slice 1a polyglot gate canary (`make gate`) — off the npm
attack surface. Status flags here mirror the phase tracker in
[Roadmap](../../concepts/roadmap.md#status-tracker).

### Open Questions

- Confirm the TinyGo CM path end-to-end at the Phase 1 gate (`wkg` dependency
  resolution, the `wasi:cli` world assumption; GC caveat avoided by using TinyGo).
- Define the concrete CI hygiene checklist (SBOM tool, per-ecosystem audit tooling).
- Revisit Kotlin/JVM once Kotlin/Wasm leaves Beta and WASI threading lands.

## Next

1. Record language policy in [Roadmap](../../concepts/roadmap.md) + add status tracker.
2. At Phase 1 gate: TinyGo toolchain + supply-chain CI.
3. Apply decision per extension as built.
