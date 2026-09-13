---
type: guide
title: Development Environment Setup
description: Install and verify the toolchain needed to build the Rust core and TinyGo guest extensions for Phase 1.
tags: [setup, toolchain, rust, tinygo, wasmtime, wit, component-model, phase-1]
created: 2026-06-29
updated: 2026-09-11
---

# Development Environment Setup

Install the toolchain to build Jan-Klod from source: **Rust core** (Wasmtime host),
**TinyGo guest extensions**, and **WIT** tooling. This is what the
[Phase 1 plan](../decisions/2026-06-29-extension-technologies/PLAN.md) assumes.

> Commands target **macOS** with inline Linux notes. This guide expands as later phases add tools.

## At a glance

| Tool | Purpose | Required for | Install via |
|---|---|---|---|
| **Rust** (`rustc`, `cargo`) ≥ 1.83 | builds `core`; embeds the Wasmtime crate | always | `rustup` |
| **Go** ≥ 1.23 | toolchain TinyGo builds on | TinyGo guests | `brew` |
| **TinyGo** ≥ 0.34 | compiles Go guests to **components** (`wasip2`) | guest extensions | `brew` |
| **wkg** ≥ 0.15 | resolves/fetches WIT package dependencies | building guests against `wit/` | `cargo install` |
| **wasm-tools** | inspect/validate components; `make extensions` reads a component's imports through it to generate each guest's capability manifest | **required to build guests** | `cargo install` |
| `wasmtime` CLI ≥ 46 | run a component standalone | optional (debug only) | `brew` / installer |
| `cargo-component` | build **Rust** guests as components | optional (Rust extensions) | `cargo install` |

The core embeds Wasmtime as a library, so the CLI is optional — it's only useful for inspecting `.wasm` files.

## Install

### 1. Rust (core)

Use `rustup` to manage Rust — it keeps `rustc`, `cargo`, and components updatable.

```shell
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
rustup component add clippy rustfmt
```

### 2. Go + TinyGo (guest extensions)

TinyGo (not standard Go) is the guest compiler — standard Go lacks Component Model
support. TinyGo **0.34+** emits components for `wasip2`.

```shell
brew install go
brew tap tinygo-org/tools
brew install tinygo
```

On Linux, download the release archive from
<https://github.com/tinygo-org/tinygo/releases> (or use the `.deb`).

### 3. wkg (WIT dependency resolution)

Guests build against `wit/` contracts. Cross-package imports (e.g. `wasi:*`)
are fetched by `wkg`:

```shell
cargo install wkg
```

`make setup` installs `wkg` and fetches `wit/spike/deps` (needed by `make gate`,
`make harness`, `make clippy`). Skip it and you get a clear message about the
missing directory, not a cryptic `bindgen!` error.

### 4. wasm-tools (inspect / validate)

```shell
cargo install wasm-tools
```

Inspects components and validates output; the `wit:` Makefile target uses it.

### 5. Optional

```shell
brew install wasmtime          # run a component by hand: wasmtime run foo.wasm
cargo install cargo-component  # only if authoring a guest in Rust
```

## Verify

Confirm versions after install (Phase 1 minimums):

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

## One-time setup

Once per clone, from the repo root:

```shell
make setup   # wkg, wit/spike/deps, git hooks, supply-chain plugins
make gate    # full integration exit gate
```

`make setup` is required for `make gate`, `make harness`, `make clippy` (they
need `wit/spike/deps`). Unit tests (`make test`/`make test-core`) do not need it.

## Building a guest component (shape)

First-party guests default to Rust. TinyGo serves as a polyglot canary: `make gate`
tests both the committed `spike.wasm` and rebuilds it from source (if `tinygo`
and `wkg` are installed), verifying WIT changes across toolchains.

`spike.wasm` is the only guest component tracked in the repo, built with
`-no-debug -opt=z` (~75 KB). Regenerate it with `make -C src/extensions spike-guest`
and commit the result; `make -C src/extensions clean` preserves it.

### Rust (default)

Since Rust 1.82, `wasm32-wasip2` emits components directly. A guest is a `cdylib`
implementing the world's `Guest` traits. Examples: `src/extensions/tool-find/`
(uses `host-fs`) and `src/extensions/provider-openai/` (uses `host-http`):

```shell
rustup target add wasm32-wasip2          # one-time
cargo build --release --target wasm32-wasip2
# -> target/wasm32-wasip2/release/<name>.wasm  (a component)
```

`make tool-find` or `make provider-openai` stage the component in `ext/`.
Use `make probe` to test a provider against a live endpoint (needs API key + network).

### TinyGo

Component-Model build path:

```shell
tinygo build -target=wasip2 \
  --wit-package ../../wit \
  --wit-world <world-name> \
  -o <name>.wasm main.go
```

> **Known quirk:** TinyGo's `wasip2` assumes `wasi:cli` — custom worlds must
> `include wasi:cli/imports`. Rust's `wasm32-wasip2` does not have this constraint.

## Supply-chain / CI tooling

The build pipeline is unprotected, so CI gates use:

```shell
cargo install cargo-deny        # Rust deps/licenses/advisories
cargo install cargo-cyclonedx  # SBOM generation (Rust workspace, CycloneDX JSON)
```

`govulncheck` for Go is invoked via `go run golang.org/x/vuln/cmd/govulncheck@latest`.

These run in CI, not locally — listed here for completeness.
