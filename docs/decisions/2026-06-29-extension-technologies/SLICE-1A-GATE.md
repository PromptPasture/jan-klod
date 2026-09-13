---
type: decision
title: Slice 1a Gate — Verdict
description: Go/no-go verdict for the Rust + Wasmtime + Component Model + TinyGo foundation, with the wasi:cli quirk resolution and the host async-model decision.
tags: [phase-1, slice-1a, gate, rust, wasmtime, component-model, tinygo, wkg, async]
created: 2026-06-29
updated: 2026-06-29
---

# Slice 1a Gate — Verdict: **PASS**

Minimal vertical slice works end-to-end: sync Rust+Wasmtime host loads TinyGo component, calls across Component Model.

**Proceed to [Slice 1b](PLAN.md#slice-1b--build-out-to-mvp-parity).** Escape hatch (Go+wazero+JSON) unneeded; CM-in-Rust and TinyGo toolchain both clean.

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

## `wasi:cli` quirk

TinyGo needs `wasi:cli/*`, `wasi:io`, `filesystem`, `random` at startup. Custom world replaces default; must re-declare imports or resolution fails.

**Fix:** (1) Guest world includes `wasi:cli/imports@0.2.0`. (2) `wkg wit fetch` populates deps (lock-pinned). (3) Host calls `add_to_linker_sync`; state implements `WasiView`.

Note: TinyGo still needs `func main()` even for reactor component.

## Async-model decision

**Baseline: sync Wasmtime.** Uses `Linker::instantiate` / `call_complete`, no `tokio`. Works for Slice 1a + core skeleton 1b.

**`tokio` at `host-http` (Slice 1b).** HTTP is inherently async; host calls async Wasmtime (`call_async` + `Config::async_support`), bridges to `tokio` reactor. Sync keeps boundary clean. Resolves [open question](../2026-06-29-component-model-rust/Handoff.md#open-questions-carried-forward--new).

## Toolchain proven

`rustc 1.96.0`, `wasmtime`/`wasmtime-wasi` 46, `tinygo 0.41.1`,
`wit-bindgen-go v0.7.0` (+ `go.bytecodealliance.org/cm v0.3.0`), `wkg 0.15.1`,
`wasm-tools 1.252.0`. Matches the [development setup](../../guides/development-setup.md).

## Reproduce

```sh
make gate   # prints: echo: hello, component model
```

## Spike vs Roadmap

[Roadmap](../../concepts/roadmap.md) says "no throwaway spike" (risk front-loaded, not separate phase). Slice 1a is explicitly a trivial stub; [PLAN.md](PLAN.md) permits throwaway `spike` world. Used here: stub inside Phase 1, deleted after verdict.

## Cleanup (at the start of Slice 1b)

The spike is disposable. When the real `provider`/`store` components land, delete
`src/extensions/spike/`, `wit/spike/`, and the host's spike `bindgen!` path +
`Spike` call. The Cargo workspace, the `wasmtime`/`wasmtime-wasi` wiring, the
`WasiView` host state, and the `wkg`/TinyGo build recipe all carry forward.
