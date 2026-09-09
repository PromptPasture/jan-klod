---
type: guide
title: Development Environment Setup
description: Install and verify the toolchain needed to build the Rust core and TinyGo guest extensions for Phase 1.
tags: [setup, toolchain, rust, tinygo, wasmtime, wit, component-model, phase-1]
created: 2026-06-29
updated: 2026-07-02
---

# Development Environment Setup

What you need installed to build Jan-Klod from source: the **Rust core** (Wasmtime
component host), **TinyGo guest extensions**, and the **WIT** tooling that ties them
together. This is the toolchain the [Phase 1 plan](../decisions/2026-06-29-extension-technologies/PLAN.md)
assumes — see the [Roadmap](../concepts/roadmap.md) for why each piece exists.

> Commands below target **macOS**. Linux notes are inline where they
> differ. This guide grows as later phases add tools (SQLite, HTTP, etc.).

## At a glance

| Tool | Purpose | Required for | Install via |
|---|---|---|---|
| **Rust** (`rustc`, `cargo`) ≥ 1.83 | builds `core`; embeds the Wasmtime crate | always | `rustup` |
| **Go** ≥ 1.23 | toolchain TinyGo builds on | TinyGo guests | `brew` |
| **TinyGo** ≥ 0.34 | compiles Go guests to **components** (`wasip2`) | guest extensions | `brew` |
| **wkg** ≥ 0.15 | resolves/fetches WIT package dependencies | building guests against `wit/` | `cargo install` |
| **wasm-tools** | inspect/validate/compose components | always (debugging) | `cargo install` |
| `wasmtime` CLI ≥ 46 | run a component standalone | optional (debug only) | `brew` / installer |
| `cargo-component` | build **Rust** guests as components | optional (Rust extensions) | `cargo install` |

The core embeds Wasmtime as a **library crate**, so the `wasmtime` CLI is *not*
required to run Jan-Klod — only handy for poking at a `.wasm` by hand.

## Install

### 1. Rust (core)

Use `rustup` to manage your Rust toolchain — it keeps `rustc`, `cargo`, and
components (`clippy`, `rustfmt`) updatable together.

```shell
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup component add clippy rustfmt
```

### 2. Go + TinyGo (guest extensions)

TinyGo (not standard Go) is our guest compiler — standard Go's GC/tagged-union
support for the Component Model isn't there yet. TinyGo **0.34+** emits a real
component when targeting `wasip2`.

```shell
brew install go
brew tap tinygo-org/tools
brew install tinygo
```

On Linux, download the release archive from
<https://github.com/tinygo-org/tinygo/releases> (or use the `.deb`).

### 3. wkg (WIT dependency resolution)

Guests build against the contracts in `wit/`. Any cross-package imports (e.g.
`wasi:*`) are fetched/locked by `wkg`.

```shell
cargo install wkg
```

Prebuilt binaries are also on
<https://github.com/bytecodealliance/wasm-pkg-tools/releases>.

### 4. wasm-tools (inspect / validate)

```shell
cargo install wasm-tools
```

Used to print a component's WIT (`wasm-tools component wit foo.wasm`) and to
validate output — the `wit:` Makefile target already calls it.

### 5. Optional

```shell
brew install wasmtime          # run a component by hand: wasmtime run foo.wasm
cargo install cargo-component  # only if authoring a guest in Rust
```

## Verify

After installing, confirm versions (these are the minimums Phase 1 expects):

```shell
rustc --version        # ≥ 1.83  (verified: 1.96.0)
cargo --version
go version             # ≥ 1.23  (verified: 1.26.4)
tinygo version         # ≥ 0.34  (verified: 0.41.1)
wkg --version          #         (verified: 0.15.1)
wasm-tools --version   #         (verified: 1.252.0)
wasmtime --version     # optional (verified: 46.0.1)
```

A green run of all six means the Slice 1a toolchain is ready. Then build per the
project [Makefile](../../Makefile) (`cargo build` for core; `tinygo build` per
guest — targets land in Slice 1b).

## Building a guest component (shape)

First-party guests **default to Rust** (see the
[extension-technology decision](../decisions/2026-06-29-extension-technologies/BRAINSTORM.md));
TinyGo is the case-by-case exception and the polyglot canary: `make gate` loads
the committed Go-built `spike.wasm` and calls across the boundary, and — if you
have `tinygo` and `wkg` installed — rebuilds it from source **into a temp dir**
and calls that too, leaving the committed copy alone (announced as a skip if you
do not). So a WIT change that a non-Rust toolchain cannot express fails on the
machine of anyone carrying that toolchain, rather than nowhere.

That `spike.wasm` is the one guest component the repository tracks, built with
`-no-debug -opt=z` (~75 KB) so committing it is reasonable. `make -C
src/extensions clean` does not delete it; if you regenerate it with `make -C
src/extensions spike-guest`, commit the result.

### Rust (default)

No `cargo-component` needed: since Rust 1.82 the `wasm32-wasip2` target emits a
**component** directly, and the `wit-bindgen` crate generates the guest bindings
from our `wit/`. A guest is a `cdylib` that implements the exported world's
`Guest` traits — see `src/extensions/tool-find/` (a leaf tool, no host capability
beyond `host-fs`) and `src/extensions/provider-openai/` (network: an
OpenAI-compatible `llm-provider` over `host-http`):

```shell
rustup target add wasm32-wasip2          # one-time
cargo build --release --target wasm32-wasip2
# -> target/wasm32-wasip2/release/<name>.wasm  (a component)
```

`make tool-find` / `make provider-openai` wrap this and stage the result in
`ext/`. To drive a provider's full `complete` path against a live endpoint,
`make probe` (needs the provider's api-key env + network).

### TinyGo (gate canary / case-by-case)

The native Component-Model path TinyGo uses against our contracts:

```shell
tinygo build -target=wasip2 \
  --wit-package ../../wit \
  --wit-world <world-name> \
  -o <name>.wasm main.go
```

> **Known quirk (tracked in [PLAN.md](../decisions/2026-06-29-extension-technologies/PLAN.md)):**
> TinyGo's `wasip2` target assumes a `wasi:cli` world — a custom `--wit-world`
> must `include wasi:cli/imports`. Rust's `wasm32-wasip2` has no such quirk.

## Supply-chain / CI tooling (Slice 1b)

Per the [extension-technology policy](../decisions/2026-06-29-extension-technologies/BRAINSTORM.md),
the build pipeline is not protected by the runtime sandbox, so the CI gates use:

```shell
cargo install cargo-deny        # Rust deps/licenses/advisories
cargo install cargo-cyclonedx  # SBOM generation (Rust workspace, CycloneDX JSON)
```

`govulncheck` for Go modules is invoked via `go run golang.org/x/vuln/cmd/govulncheck@latest` — no install needed.

These run in CI, not as a local prerequisite — listed here so the full picture is
in one place.
