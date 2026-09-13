# Phase 1 Plan — Walking Skeleton + Foundation Gate

Living checklist for Phase 1 of [Roadmap](../../concepts/roadmap.md). Update flags as work proceeds.

[Component Model](../2026-06-29-component-model-rust/Handoff.md) · [Extension Tech](BRAINSTORM.md).

## Repo layout

```
src/
  core/       # Rust host (Wasmtime + Component Model). Workspace root.
  extensions/ # one dir per guest (Rust by default)
wit/          # language-neutral contracts (repo root, polyglot consumption)
```

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Item | Flag |
|---|---|
| Slice 1a — gate | `done` — **PASSED** ([verdict](SLICE-1A-GATE.md)) |
| Slice 1b — MVP parity | `done` — core skeleton + 3 host caps (`host-log`/config/http); 3 guests (`store-memory`, `provider-openai`, `manager-agent-loop`); component harness verifies offline. **Gate met:** sandboxed `manager-agent-loop` runs one turn (complete → store) with core routing its imports into implementations (`jan_klod_core::route`), proven by `tests/routing.rs`. Agent loop in guest per [architecture](../../concepts/architecture.md). Supply-chain CI ([`.github/workflows/ci.yml`](../../../.github/workflows/ci.yml)): lint + harness + audits + SBOM via `make`. **Phase 1 done.** |

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

- [x] Core skeleton: `jan-klod.yaml` loader; registry + boot ordering (category-tier); lifecycle (`init`→`start`); component host `ext/*.wasm`. Category-neutral `extension-world` (imports all host-caps, exports `lifecycle`) drives all guests uniformly.
- [x] Host caps: `host-log`, `host-config`, `host-http` ✓. HTTP is blocking (`ureq` 3, sync, no `tokio`; async contingency unneeded). Capability-neutral impl in [`jan_klod_core::http`](../../../src/core/core/src/http.rs); 4xx/5xx/transport → typed errors.
- [x] `store-memory` (Rust) — real component, HashMap backend. `wit-bindgen` crate + `wasm32-wasip2` target (component directly, no `cargo-component`). Loads, drives lifecycle, round-trips host-caps. Loads via category-neutral `extension-world`.
- [x] `provider-openai` (Rust) — OpenAI-compatible LLM over `host-http`. One blocking fetch (non-streaming, host buffers), parses into chunks (tool-calls or text-delta) under stream handles. Status/transport → typed errors. Default endpoint, OpenAI-compatible servers differ by config (url/key/model). Proven by `provider_probe`.
- [x] Routing (`jan_klod_core::route`) + agent-loop guest — lets extensions consume others' interfaces. Guest imports `host-log`/`llm-provider`/`memory-store`, exports lifecycle + minimal `run(prompt) → string`. Core brokers the provider/store, satisfies imports by delegating (sync, types converted). `Runtime::route_agent_loop` resolves, wires, drives lifecycles. Injected `host-http` allows live/offline. v0 hand-wired for two deps; generalized when needed.
- [x] Build: `Makefile` — `make run` boots the core; `make gate` reproduces Slice 1a (now an example); `make store-memory` / `make provider-openai` / `make manager-agent-loop` build the guests (`cargo build --target wasm32-wasip2`), `*-docker` variants are the no-rustup container fallback (`rust:1-slim`); `make probe` runs the live-completion probe; `make harness` runs the offline component + routing tests; `tinygo`+`wkg` retained only for the Slice 1a gate canary
- [x] Supply-chain CI gates: a `.github/workflows/ci.yml` that installs the tooling and then calls the same `make` targets a developer runs locally. **Rust** — `make audit` (`cargo-audit`, RUSTSEC) and `make deny` (`cargo-deny`) run over the host workspace *and* every guest crate (each a standalone `Cargo.lock`); one shared [`deny.toml`](../../../deny.toml) at the repo root holds the license / advisory / source policy (license `allow` list derived from `cargo metadata` — copyleft/unknown denied by omission); CI also asserts every `Cargo.lock` is current with `cargo metadata --locked`. **Go** (Slice 1a canary only) — `make -C src/extensions go-supply-chain` globs every `src/extensions/*/go.mod` and runs `go mod verify` + `govulncheck ./...` under `GOFLAGS=-mod=readonly`. **SBOM** — `make sbom` (`cargo-cyclonedx`, CycloneDX JSON) covers the Rust workspace; Go modules are excluded (only the Slice 1a canary exists and is covered by `govulncheck`). Uploaded as a CI artifact. Verified locally where tooling exists (`cargo-audit` clean across all four crates, all locks current, `go mod verify` passes); `cargo-deny`/`cargo-cyclonedx` are not installed on the dev host and execute first on the initial CI run. `govulncheck` is invoked via `go run` — no install required.
- [x] Tests: `cargo test` ✓ (14 host-side; ureq status-mapping + config/boot). Component-level harness ([src/core/host/tests/component_harness.rs](../../../src/core/host/tests/component_harness.rs)) — the generalization of the `provider_probe` example into the "load a guest, verify its WIT interface" harness the plan called for: it binds both category worlds, backs their imports with one reusable `TestHost` (config section, captured logs, a **canned** `host-http` so the provider runs offline), then verifies each staged guest end-to-end — `store-memory` lifecycle + full `memory-store` round-trip, and `provider-openai` lifecycle + `complete` (canned 200 → text-delta/done, and 401 → `auth-failed`). Each test skips when its component is not staged, so a bare `cargo test` stays green; `make harness` builds the guests then runs them. The `provider_probe` example remains the **live**-endpoint path (`make probe`)
- [x] **Exit gate (met):** config-driven load → lifecycle → OpenAI-compatible completion through a sandboxed component + in-memory store, all over the Component Model — driven by the sandboxed `manager-agent-loop` guest with **core only routing**. Proven offline by [`tests/routing.rs`](../../../src/core/host/tests/routing.rs): it boots the real `Runtime` against a `jan-klod.yaml`, calls `route_agent_loop` (canned `host-http`, no network/api key), and runs one turn — the guest completes the prompt through its routed `llm-provider`, persists it through its routed `memory-store`, reads it back, and returns the text. Every property the gate asks for now holds with the agent loop **in the guest where [architecture](../../concepts/architecture.md) mandates it** ("zero agent behaviour in core … the agent loop itself is the `manager-agent-loop` extension"), not in host code. `provider_probe` remains the live single-leg path (`make probe`); the `agent-manager` streaming interface stays the north-star contract for the full orchestrator (routing/fallback/tools/context), unbuilt.

## Cross-cutting (from day one)

- Structured logging
- **Rust lint policy** — `[workspace.lints]` in `src/core/Cargo.toml`, inherited by
  every crate: clippy `all`/`pedantic`/`nursery` = `warn`, `unsafe_code = "deny"`,
  `missing_docs = "warn"`. Generated `bindgen!` output is scoped out with a local
  `#[allow(...)]`. Workspace is warning-clean; CI enforces `-D warnings` via the
  `lint-test` job (`make clippy`) in [`.github/workflows/ci.yml`](../../../.github/workflows/ci.yml).
- The supply-chain CI gates above
