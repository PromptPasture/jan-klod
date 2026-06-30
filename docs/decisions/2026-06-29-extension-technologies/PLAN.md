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
| Slice 1b — MVP parity | `in-progress` — core skeleton + all three host caps (`host-log`/`host-config`/`host-http`) done; both guests built (`store-memory` + `provider-openai`); a live OpenAI-compatible completion runs end-to-end via `provider_probe`; supply-chain CI gates + a proper component test harness remain |

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

- [x] Rust core skeleton: `jan-klod.yaml` loader (`jan-klod-config`); extension registry + boot ordering (category-tier; full dependency-graph deferred until managers declare deps); lifecycle drive (`init`→`start`); component host loading `ext/*.wasm` — `jan-klod-core` crate, `Runtime::boot`/`start_all`. The core instantiates every guest as the category-neutral `extension-world` (imports the full host-cap set, exports only `extension-lifecycle`) so one target drives lifecycle on any guest, store or provider, without depending on a category interface.
- [x] Host capabilities as CM imports: `host-log` ✓ + `host-config` ✓ + `host-http` ✓. `host-http` is now a real **blocking** client — `ureq` 3 (synchronous, rustls TLS, **no `tokio`**: the async-at-host-http contingency was not needed; the sync Wasmtime baseline holds). Implemented capability-neutral in [`jan_klod_core::http`](../../../src/core/core/src/http.rs) (plain types) so both the core's `host-http::Host` and the `provider_probe` example adapt onto one impl; 4xx→`client-error`, 5xx→`server-error`, transport failures→matching variant.
- [x] `store-memory` (Rust) — real `memory-store` component, in-memory `HashMap` backend. Built with the `wit-bindgen` crate + the `wasm32-wasip2` target (emits a component directly; **no `cargo-component` needed**). Loads through the core, drives `init`→`start`, and round-trips both host caps it imports (`host-log` lines tagged `[store.memory]`, `host-config` `all()` returns its section). Loads via the category-neutral `extension-world` (see the core-skeleton item above).
- [x] `provider-openai` (Rust) — OpenAI-compatible `llm-provider` over `host-http`. `complete` builds a Chat Completions request, issues **one blocking `host-http::fetch`** (non-streaming — the host buffers the whole body), parses `choices[0].message` into ordered chunks (`tool-call-request`s or a `text-delta`, closed by `done`) buffered under a stream handle the host drains via `next-chunk`. Status/transport errors → `provider-error` (401/403→`auth-failed`, 404→`model-not-found`, 429→`rate-limited`, else `transient`). `type: openai` is the default, so one build serves every OpenAI-compatible endpoint (LM Studio, Groq, vLLM, …); only `base-url`/`api-key`/`model` (from `host-config`) differ. **End-to-end proven** by `provider_probe` (below).
- [x] Build: `Makefile` — `make run` boots the core; `make gate` reproduces Slice 1a (now an example); `make store-memory` / `make provider-openai` build the guests (`cargo build --target wasm32-wasip2`), `*-docker` variants are the no-rustup container fallback (`rust:1-slim`); `make probe` runs the live-completion probe; `tinygo`+`wkg` retained only for the Slice 1a gate canary
- [ ] Supply-chain CI gates: Rust `Cargo.lock` + `cargo-deny`/`cargo-audit` (primary, all our extensions); Go `-mod=readonly` + `go.sum` verify + `govulncheck` (Slice 1a gate spike only); SBOM (`syft`)
- [~] Tests: `cargo test` ✓ (14 host-side; ureq status-mapping + config/boot). Component-level: the `provider_probe` example ([src/core/host/examples/provider_probe.rs](../../../src/core/host/examples/provider_probe.rs)) instantiates `provider-world`, runs lifecycle, and drives a real `complete` over `host-http` — the seed of a proper harness; a generalized "load a guest, verify its WIT interface" harness still to come
- [~] **Exit gate:** config-driven load → lifecycle → OpenAI-compatible completion through a sandboxed component + in-memory store, all over the Component Model → Phase 1 `done`. Every piece now exists in isolation (config load ✓, lifecycle ✓, sandboxed completion over CM ✓ via the probe, in-memory store ✓); the remaining work is wiring them into one driven flow (needs the agent-loop manager + a harness) rather than the probe's hand-rolled caller.

## Cross-cutting (from day one)

- Structured logging
- **Rust lint policy** — `[workspace.lints]` in `src/core/Cargo.toml`, inherited by
  every crate: clippy `all`/`pedantic`/`nursery` = `warn`, `unsafe_code = "deny"`,
  `missing_docs = "warn"`. Generated `bindgen!` output is scoped out with a local
  `#[allow(...)]`. Workspace is warning-clean; CI will enforce `-D warnings` (below).
- The supply-chain CI gates above
