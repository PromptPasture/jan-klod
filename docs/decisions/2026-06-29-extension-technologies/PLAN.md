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
| Slice 1b — MVP parity | `done` — core skeleton + all three host caps (`host-log`/`host-config`/`host-http`) done; three guests built (`store-memory` + `provider-openai` + `manager-agent-loop`); the component harness verifies each guest offline; the **exit gate is met for real** — the `manager-agent-loop` *extension* runs one turn (completion → store) with **core only routing** its `llm-provider`/`memory-store` imports into the implementing extensions (`jan_klod_core::route`), proven by `tests/routing.rs`. The agent loop is a sandboxed guest, not core, per [architecture](../../concepts/architecture.md). The supply-chain CI gates now close the slice: [`.github/workflows/ci.yml`](../../../.github/workflows/ci.yml) enforces lint + harness + `cargo-audit`/`cargo-deny`/`govulncheck` + SBOM, all via `make`. **Phase 1 done.** |

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
- [x] Inter-component routing (`jan_klod_core::route`) + `manager-agent-loop` guest — the core capability that lets one extension consume another's interface, and the v0 agent loop that exercises it. The guest is authored against a reduced `agent-loop-world` (imports `host-log` + `llm-provider` + `memory-store`, exports `extension-lifecycle` + a minimal `agent-loop.run(prompt) -> string`); `host-config`/`context-manager`/tools/fallback are deferred to the future `agent-manager`. The core **broker** instantiates the provider + store, then satisfies the manager's *imported* `llm-provider`/`memory-store` by delegating each call into the implementing instance (cross-store, sync; stream handles pass through; the structurally-identical generated types are converted at the boundary). `Runtime::route_agent_loop` resolves the enabled manager/provider/store, wires the routing, drives all three lifecycles, and returns a `RoutedAgentLoop`; `run` forwards to the guest. The provider's `host-http` is injected (`route::HttpFn`) so a turn runs live or offline. **YAGNI:** v0 is hand-wired for the two deps — generalised only when a third appears.
- [x] Build: `Makefile` — `make run` boots the core; `make gate` reproduces Slice 1a (now an example); `make store-memory` / `make provider-openai` / `make manager-agent-loop` build the guests (`cargo build --target wasm32-wasip2`), `*-docker` variants are the no-rustup container fallback (`rust:1-slim`); `make probe` runs the live-completion probe; `make harness` runs the offline component + routing tests; `tinygo`+`wkg` retained only for the Slice 1a gate canary
- [x] Supply-chain CI gates: a `.github/workflows/ci.yml` that installs the tooling and then calls the same `make` targets a developer runs locally. **Rust** — `make audit` (`cargo-audit`, RUSTSEC) and `make deny` (`cargo-deny`) run over the host workspace *and* every guest crate (each a standalone `Cargo.lock`); one shared [`deny.toml`](../../../deny.toml) at the repo root holds the license / advisory / source policy (license `allow` list derived from `cargo metadata` — copyleft/unknown denied by omission); CI also asserts every `Cargo.lock` is current with `cargo metadata --locked`. **Go** (Slice 1a canary only) — `make -C src/extensions go-supply-chain` globs every `src/extensions/*/go.mod` and runs `go mod verify` + `govulncheck ./...` under `GOFLAGS=-mod=readonly`. **SBOM** — `make sbom` (`syft`, SPDX-JSON) uploaded as a CI artifact. Verified locally where tooling exists (`cargo-audit` clean across all four crates, all locks current, `go mod verify` passes); `cargo-deny`/`govulncheck`/`syft` are not installed on the dev host and execute first on the initial CI run.
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
