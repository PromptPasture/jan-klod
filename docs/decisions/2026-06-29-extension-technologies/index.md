# Extension Technologies (2026-06-29)

Language/toolchain policy for our own first-party extensions.

- [Brainstorm](BRAINSTORM.md) — selection rule (maturity + supply-chain gated, per-extension best-fit), the language menu (default Go/Rust; TS/Python case-by-case; Kotlin excluded for now), and provisional near-term assignments.
- [Plan](PLAN.md) — living Phase 1 execution checklist (walking skeleton + foundation gate); decided `src/` repo layout; status flags mirroring the roadmap tracker.
- [Slice 1a Gate](SLICE-1A-GATE.md) — go/no-go verdict (**PASS**, 2026-06-29): Rust + Wasmtime loads a TinyGo component over the Component Model; the `wasi:cli` quirk resolution; the host async-model decision (sync baseline, `tokio` at `host-http`).
