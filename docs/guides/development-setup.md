---
type: guide
title: Development Environment Setup
description: Install and verify the toolchain needed to build the Rust core and TinyGo guest extensions for Phase 1.
tags: [setup, toolchain, rust, tinygo, wasmtime, wit, component-model, phase-1]
created: 2026-06-29
updated: 2026-06-29
---

# Development Environment Setup

What you need installed to build Jan-Klod from source: the **Rust core** (Wasmtime
component host), **TinyGo guest extensions**, and the **WIT** tooling that ties them
together. This is the toolchain the [Phase 1 plan](../decisions/2026-06-29-extension-technologies/PLAN.md)
assumes — see the [Roadmap](../concepts/roadmap.md) for why each piece exists.

> Commands below target **macOS** (Homebrew). Linux notes are inline where they
> differ. This guide grows as later phases add tools (SQLite, HTTP, etc.).

## At a glance

| Tool | Purpose | Required for | Install via |
|---|---|---|---|
| **Rust** (`rustc`, `cargo`) ≥ 1.83 | builds `core`; embeds the Wasmtime crate | always | `rustup` |
| **Go** ≥ 1.23 | toolchain TinyGo builds on | TinyGo guests | `brew` |
| **TinyGo** ≥ 0.34 | compiles Go guests to **components** (`wasip2`) | guest extensions | `brew` |
| **wkg** ≥ 0.15 | resolves/fetches WIT package dependencies | building guests against `wit/` | `cargo install` |
| **wasm-tools** | inspect/validate/compose components | always (debugging) | `cargo install` / `brew` |
| `wasmtime` CLI ≥ 46 | run a component standalone | optional (debug only) | `brew` / installer |
| `cargo-component` | build **Rust** guests as components | optional (Rust extensions) | `cargo install` |

The core embeds Wasmtime as a **library crate**, so the `wasmtime` CLI is *not*
required to run Jan-Klod — only handy for poking at a `.wasm` by hand.

## Install

### 1. Rust (core)

Use `rustup`, not Homebrew's `rust` — it keeps the toolchain and components
(`clippy`, `rustfmt`) updatable together.

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
cargo install wasm-tools   # or: brew install wasm-tools
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

The native Component-Model path TinyGo uses against our contracts:

```shell
tinygo build -target=wasip2 \
  --wit-package ../../wit \
  --wit-world <world-name> \
  -o <name>.wasm main.go
```

> **Known quirk (tracked in [PLAN.md](../decisions/2026-06-29-extension-technologies/PLAN.md)):**
> TinyGo's `wasip2` target assumes a `wasi:cli` world. Slice 1a's gate exists
> partly to settle how this interacts with our worlds before we build out.

## Supply-chain / CI tooling (Slice 1b)

Per the [extension-technology policy](../decisions/2026-06-29-extension-technologies/BRAINSTORM.md),
the build pipeline is not protected by the runtime sandbox, so the CI gates use:

```shell
go install golang.org/x/vuln/cmd/govulncheck@latest   # Go guest vuln scan
cargo install cargo-deny                               # Rust deps/licenses/advisories
brew install syft                                      # SBOM generation
```

These run in CI, not as a local prerequisite — listed here so the full picture is
in one place.
