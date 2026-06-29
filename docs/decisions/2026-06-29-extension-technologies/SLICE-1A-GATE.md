---
type: decision
title: Slice 1a Gate — Verdict
description: Go/no-go verdict for the Rust + Wasmtime + Component Model + TinyGo foundation, with the wasi:cli quirk resolution and the host async-model decision.
tags: [phase-1, slice-1a, gate, rust, wasmtime, component-model, tinygo, wkg, async]
created: 2026-06-29
updated: 2026-06-29
---

# Slice 1a Gate — Verdict: **PASS**

The thinnest possible vertical slice works end-to-end: a synchronous Rust +
Wasmtime host loads a **TinyGo-built component**, built against a **custom WIT
world**, and calls its exported function across the Component Model boundary.

**Decision: proceed to [Slice 1b](PLAN.md#slice-1b--build-out-to-mvp-parity).**
The documented escape hatch (fall back to Go + wazero + JSON ABI) is **not**
exercised — CM-in-Rust and the TinyGo CM toolchain are both clean.

## What was built

| Path | Role |
|---|---|
| `wit/spike/world.wit` | Throwaway gate world: `export complete: func(prompt: string) -> string`, plus `include wasi:cli/imports@0.2.0`. Separate package (`jan-klod:spike`) — not part of canonical `jan-klod:interfaces`. |
| `src/core/` | Cargo workspace; `host` crate → `jan-klod` binary. Embeds `wasmtime` (sync) + `wasmtime-wasi`; `bindgen!` generates the typed guest binding. |
| `src/extensions/spike/` | TinyGo guest. `wit-bindgen-go` bindings + a 3-line `Exports.Complete` that echoes. |
| `Makefile` `gate` target | Reproduces the check: `wkg wit fetch` → `tinygo build -target=wasip2` → `cargo run`. |

## Evidence

```console
$ make gate
…
echo: hello, component model
```

The component validates as a real component (`wasm-tools validate`), exports
`complete`, and imports exactly the `wasi:*` set that `wasmtime-wasi` satisfies.

## The `wasi:cli` quirk — settled

TinyGo's `wasip2` runtime needs `wasi:cli/*` (environment, stdio, clocks),
`wasi:io`, `wasi:filesystem`, and `wasi:random` imports for startup. A custom
`-wit-world` **replaces** TinyGo's default command world, so unless our world
re-declares those imports, `wasm-tools component new` fails to resolve them
(observed: `failed to resolve import wasi:cli/environment@0.2.0`).

Resolution — three coordinated pieces:

1. **Guest world** `include wasi:cli/imports@0.2.0;` so the imports are present.
2. **`wkg wit fetch`** populates `wit/spike/deps/` with the wasi packages
   (lock-pinned; deps are git-ignored and re-fetched).
3. **Host** calls `wasmtime_wasi::p2::add_to_linker_sync` to provide them; store
   state implements `WasiView`.

No `func main()` is dropped — TinyGo's `wasip2` target still requires it even for
a reactor-style component whose real entry point is the exported function.

## Async-model decision (resolves a Phase 1 open question)

**Baseline: synchronous Wasmtime.** The host uses `Linker::instantiate` /
`call_complete` with no `tokio` runtime. This is enough for Slice 1a and for the
core's config/registry/lifecycle skeleton in Slice 1b.

**Where `tokio` becomes necessary:** `host-http` (Slice 1b). An outbound HTTP
capability is inherently async, and `wasmtime-wasi`'s HTTP/socket support is
built around `tokio`. At that point the host calls into the guest on async
Wasmtime (`call_async` + `Config::async_support(true)`) and bridges the
`host-http` import to a `tokio` reactor. Until then, sync keeps the host simple
and the boundary obvious. This resolves the async bullet in the
[foundation decision's open questions](../2026-06-29-component-model-rust/Handoff.md#open-questions-carried-forward--new).

## Toolchain proven

`rustc 1.96.0`, `wasmtime`/`wasmtime-wasi` 46, `tinygo 0.41.1`,
`wit-bindgen-go v0.7.0` (+ `go.bytecodealliance.org/cm v0.3.0`), `wkg 0.15.1`,
`wasm-tools 1.252.0`. Matches the [development setup](../../guides/development-setup.md).

## Reproduce

```sh
make gate   # prints: echo: hello, component model
```

## Note on "spike" vs the roadmap wording

The [roadmap](../../concepts/roadmap.md) Phase 1 goal says "no throwaway spike",
meaning Phase 1 is not a *separate* discardable phase — its risk is front-loaded
into Slice 1a. Slice 1a itself is explicitly "a trivial stub … echoes a single
`complete`", and [PLAN.md](PLAN.md) permitted "a throwaway `spike` world". This
gate uses exactly that: a stub, inside the real Phase 1, deleted once the verdict
is recorded.

## Cleanup (at the start of Slice 1b)

The spike is disposable. When the real `provider`/`store` components land, delete
`src/extensions/spike/`, `wit/spike/`, and the host's spike `bindgen!` path +
`Spike` call. The Cargo workspace, the `wasmtime`/`wasmtime-wasi` wiring, the
`WasiView` host state, and the `wkg`/TinyGo build recipe all carry forward.
