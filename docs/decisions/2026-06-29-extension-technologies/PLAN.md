# Phase 1 Plan — Walking Skeleton + Foundation Gate

Living execution checklist for Phase 1 of the
[Roadmap](../../concepts/roadmap.md). Update the flags here and in the roadmap
[Status tracker](../../concepts/roadmap.md#status-tracker) as work proceeds.

Decisions & rationale:
[Component Model on Rust + Wasmtime](../2026-06-29-component-model-rust/Handoff.md) ·
[Extension Technologies](BRAINSTORM.md).

## Repo layout (decided)

All implementation code lives under `src/` (never the project root):

```
src/
  core/            # Rust host: Wasmtime + Component Model. Cargo workspace root.
  extensions/
    <name>/        # one dir per guest (Rust by default; cargo-component)
wit/               # language-agnostic WIT contracts — kept at repo root
```

`wit/` stays at the root deliberately: the contracts are language-neutral and
consumed across the polyglot `src/` tree, so they sit above any single language's
code.

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Item | Flag |
|---|---|
| Slice 1a — gate | `done` — **PASSED** ([verdict](SLICE-1A-GATE.md)) |
| Slice 1b — MVP parity | `in-progress` — core skeleton + `host-log`/`host-config` done; guests next |

## Slice 1a — the gate (go/no-go)

**Verdict: PASS** (2026-06-29). Full write-up: [SLICE-1A-GATE.md](SLICE-1A-GATE.md).
Reproduce with `make gate`.

- [x] Cargo workspace at `src/core/`; add `wasmtime` (component model) + `wasmtime::component::bindgen!`
- [x] Thin contract for the gate: throwaway `spike` world (`export complete: func(prompt: string) -> string`)
- [x] TinyGo (v0.41.1) guest: `wit-bindgen-go` bindings, `wkg` dep resolution, `wasi:cli` world quirk settled (`include wasi:cli/imports`), built to a component
- [x] Host loads the component, calls `complete`, prints the echo (`echo: hello, component model`)
- [x] Async model decision: sync Wasmtime baseline; `tokio` enters at `host-http` (1b) — see [verdict](SLICE-1A-GATE.md#async-model-decision-resolves-a-phase-1-open-question)
- [x] **Gate verdict** → trackers updated. **Pass → Slice 1b.** Escape hatch (Go + wazero) not needed.

## Slice 1b — build out to MVP parity (only after the gate passes)

- [x] Rust core skeleton: `jan-klod.yaml` loader (`jan-klod-config`); extension registry + boot ordering (category-tier; full dependency-graph deferred until managers declare deps); lifecycle drive (`init`→`start`); component host loading `ext/*.wasm` — `jan-klod-core` crate, `Runtime::boot`/`start_all`
- [~] Host capabilities as CM imports: `host-log` ✓ + `host-config` ✓ implemented; `host-http` wired into the linker as a stub (returns `backend`) until `provider-openai` needs it (then add the blocking HTTP client + `tokio` if needed)
- [ ] `store-memory` (Rust, `cargo-component`) — real `memory-store` component
- [ ] `provider-openai` (Rust, `cargo-component`) — OpenAI-compatible `llm-provider` over `host-http`
- [x] Build: `Makefile` — `make run` boots the core; `make gate` reproduces Slice 1a (now an example); per-guest `cargo component build` targets land with the first guest (`tinygo`+`wkg` retained only for the Slice 1a gate canary)
- [ ] Supply-chain CI gates: Rust `Cargo.lock` + `cargo-deny`/`cargo-audit` (primary, all our extensions); Go `-mod=readonly` + `go.sum` verify + `govulncheck` (Slice 1a gate spike only); SBOM (`syft`)
- [ ] Tests: `cargo test` + a component test harness (load a guest, verify its WIT interface)
- [ ] **Exit gate:** config-driven load → lifecycle → OpenAI-compatible completion through a sandboxed component + in-memory store, all over the Component Model → Phase 1 `done`

## Cross-cutting (from day one)

- Structured logging
- The supply-chain CI gates above
