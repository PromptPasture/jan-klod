---
type: decision
title: Go + Wazero + WASM Stack (superseded)
description: Replaced Java/Quarkus with Go + Wazero + WASM; locked stack, storage, UI, and deployment. Host language/runtime later superseded by 2026-06-29 (Rust + Wasmtime).
tags: [decision, stack, go, wazero, wasm, superseded]
created: 2026-06-28
updated: 2026-06-28
---

# Handoff — Stack Decision: Go + Wazero + WASM

> **Superseded** by [2026-06-29 — Component Model on Rust + Wasmtime](../2026-06-29-component-model-rust/Handoff.md).
> The host is Rust on Wasmtime; there is no pure-Go Component Model host, which
> is the reason recorded there. **What outlived it:** this is the record that
> replaced Java/Quarkus, and the architecture it says is unchanged — extensions,
> blue/green, configurator, the loop — is still the shape of the system. Only the
> language and the loading mechanism moved on.

## What this is

Replaced Java/Quarkus with **Go + Wazero + WASM component model**. Architecture (extensions, blue/green, configurator, loop) unchanged; only language + loading changed.

Original: [2026-06-16-jan-klod](../2026-06-16-jan-klod/)

---

## Why Go + Wazero

| Option | Issue |
|---|---|
| Java/Kotlin/Spring | ABI fragile, no sandbox, large footprint |
| Rust + Wasmtime | Best, but borrow checker too hard for AI authoring |
| **Go + Wazero** | ✅ Low hardness, good footprint, production WASM |

---

## Locked decisions

| Decision | Choice | Rationale |
|---|---|---|
| Core language | Go | Low hardness for AI-generated code; single binary; no borrow checker |
| WASM host | Wazero (pure Go, no CGo) | Production-ready; no native dependencies |
| Extension format | WASM component model + WIT | Stable ABI; language-agnostic; sandboxed |
| HTTP | `chi` + `net/http` (REST + SSE) | Curl-debuggable; streaming + browser-compatible |
| SQL | `sqlc` (generated, no ORM) | Type-safe |
| Storage | `modernc/sqlite` (pure Go) | Zero-ops; embedded |
| UI | Native Go (`ui-tui`, `ui-web`, `ui-gui`) | WASM can't access terminal; native exception |
| API | Native Go (`api-rest`, `api-grpc`, `api-graphql`) | Pluggable; user picks in config |
| Chat | Native Go (`chat-slack`, `chat-telegram`, etc.) | Long-lived connections; unified memory |
| Core | Go module + thin `main.go` | Stable; extensions update freely |
| Observability | Structured logging + Prometheus + OTEL | Bundled |
| Build | Go modules | No Gradle/Maven |
| Config format | YAML only (`jan-klod.yaml`) | Carried from original design |

---

## What changed from the original design

| Original (Java/Quarkus) | New (Go + WASM) |
|---|---|
| Group ID `com.github.janklod` | Not applicable — no Maven |
| Artifact IDs `com.github.janklod:provider-*` | Filenames `provider-*.wasm` |
| Java interface contracts | WIT interface contracts (same concepts) |
| ServiceLoader + Quarkus CDI | Wazero component loading |
| JVM classloader isolation | WASM sandbox (stronger) |
| `lib/*.jar` + `ext/*.jar` | Single Go binary + `ext/*.wasm` |
| Separate launcher binary (open question) | Core Go binary is the launcher |
| GraalVM native-image | Standard `go build` (single binary by default) |
| `bundle-*` = different GraalVM builds | `bundle-*` = same binary + different `ext/` + preset config |

---

## What carried over unchanged

- Linux kernel philosophy — core does nothing alone
- Extension taxonomy (`provider-*`, `manager-*`, `store-*`, `registry-*`, `tool-*`)
- YAML config format and `extension-name: false` toggle pattern
- Blue/green deployment (symlink flip, `state.yaml`, health-check rollback)
- SQLite as default state backend
- Configurator concept (ZIP generator, bundle presets)
- Small-model harness design (language-agnostic)
- Skills at `.agents/skills/`
- Event bus between extensions
- Agent loop architecture

---

## Resolved

1. Multi `llm-provider`: **active per-request selection**
2. Streaming: **first-class, mandatory**
3. Extension versioning: **independent**
4. MCP crashes: **mark down, remove tools, reconnect exponentially**
5. Config hot-reload: **yes, via event bus**
6. Configurator: **public (GitHub Pages → start.janklod.dev)**
7. Deployment: **desktop, ARM home, Docker, Kubernetes**
8. ACP: **new `agent-*` category; WIT `agent-delegate`**

---

## Caveats

- `wit-bindgen-go` ~1 year behind Rust; Wazero solid, component model newer.
- Kotlin/WASM not production-ready; use Rust/Go/C/JS for extensions.
- Go RSS 15–20MB vs Rust 8MB; irrelevant for network-bound agent.

---

## Next

1. **WIT contracts** — write `.wit` for core interfaces
2. **Go scaffold** — `go mod init`, layout, `wazero` + `chi`
3. **First extension** — `store-sqlite.wasm` in Rust (validate WIT roundtrip)

---

## Notes for next agent

- Do not reintroduce Java, Maven, or Quarkus. The stack decision is locked.
- Do not add CGo dependencies to the core — keep it pure Go.
- YAML config format is locked. Do not suggest JSON or TOML alternatives.
- Group ID `com.github.janklod` is obsolete — do not reference it.
- UI components (TUI, GUI) are separate binaries talking REST, not WASM extensions.
- Read [BRAINSTORM.md](../2026-06-16-jan-klod/BRAINSTORM.md) for the full original design context before making architecture decisions.
