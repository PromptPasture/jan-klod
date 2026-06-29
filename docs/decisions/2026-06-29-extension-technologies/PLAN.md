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
    <name>/        # one dir per guest (TinyGo by default)
wit/               # language-agnostic WIT contracts — kept at repo root
```

`wit/` stays at the root deliberately: the contracts are language-neutral and
consumed across the polyglot `src/` tree, so they sit above any single language's
code.

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Item | Flag |
|---|---|
| Slice 1a — gate | `not-started` |
| Slice 1b — MVP parity | `not-started` |

## Slice 1a — the gate (go/no-go)

- [ ] Cargo workspace at `src/core/`; add `wasmtime` (component model) + `wasmtime::component::bindgen!`
- [ ] Thin contract for the gate: stubbed `llm-provider` (single `complete`) or a throwaway `spike` world
- [ ] TinyGo (v0.34+) guest: `wit-bindgen-go` bindings, `wkg` dep resolution, handle the `wasi:cli` world quirk, build to a component
- [ ] Host loads the component, calls `complete`, prints the echo
- [ ] Async model decision: start with sync Wasmtime; document where `tokio` becomes necessary (host-http in 1b)
- [ ] **Gate verdict** → update both trackers. Pass → Slice 1b. Friction → `blocked` + fall back to Go + wazero per the documented escape hatch.

## Slice 1b — build out to MVP parity (only after the gate passes)

- [ ] Rust core skeleton: `jan-klod.yaml` loader; extension registry + dependency-graph boot ordering; lifecycle (`init/start/stop/health`); component host loading `ext/*.wasm`
- [ ] Host capabilities as CM imports: `host-log`, `host-config`, `host-http`
- [ ] `store-memory` (TinyGo) — real `memory-store` component
- [ ] `provider-openai` (TinyGo) — OpenAI-compatible `llm-provider` over `host-http`
- [ ] Build: revive `Makefile` — `cargo build` for core, `tinygo build` (+ `wkg`) per guest
- [ ] Supply-chain CI gates: Go `-mod=readonly` + `go.sum` verify + `govulncheck`; Rust `Cargo.lock` + `cargo-deny`/`cargo-audit`; SBOM (`syft`)
- [ ] Tests: `cargo test` + a component test harness (load a guest, verify its WIT interface)
- [ ] **Exit gate:** config-driven load → lifecycle → OpenAI-compatible completion through a sandboxed component + in-memory store, all over the Component Model → Phase 1 `done`

## Cross-cutting (from day one)

- Structured logging
- The supply-chain CI gates above
