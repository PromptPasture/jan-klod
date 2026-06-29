---
type: decision
title: Go + Wazero + WASM Stack (superseded)
description: Replaced Java/Quarkus with Go + Wazero + WASM; locked stack, storage, UI, and deployment. Host language/runtime later superseded by 2026-06-29 (Rust + Wasmtime).
tags: [decision, stack, go, wazero, wasm, superseded]
created: 2026-06-28
updated: 2026-06-28
---

# Handoff — Stack Decision: Go + Wazero + WASM

## What this is

A design session that replaced the original Java/Quarkus stack with **Go + Wazero + WASM component model**. All architectural concepts (extension model, blue/green deployment, configurator, agent loop) carry forward unchanged. Only the implementation language and extension loading mechanism changed.

The original decisions are preserved in:
- [2026-06-16-jan-klod/Handoff.md](../2026-06-16-jan-klod/Handoff.md) — original Java/Quarkus design
- [2026-06-16-jan-klod/BRAINSTORM.md](../2026-06-16-jan-klod/BRAINSTORM.md) — full original brainstorm

---

## Decision path

The session explored these options in order before landing on Go:

| Option | Rejected because |
|---|---|
| Java + Quarkus | JVM extension ABI fragile, no sandboxing |
| Kotlin + Quarkus | Mutiny vs coroutines friction; same ABI problem |
| Kotlin + Micronaut | Kotlin/WASM WASI target not production-ready |
| Spring Boot | AOT footprint too large, ~50–100ms startup |
| Rust + Wasmtime | Best footprint + WASM maturity, but borrow checker hardness unacceptable when Claude Code is primary author |
| **Go + Wazero** | ✅ Chosen — low hardness, good footprint, production-ready WASM host |

---

## Locked decisions

| Decision | Choice | Rationale |
|---|---|---|
| Core language | Go | Low hardness for AI-generated code; single binary; no borrow checker |
| WASM host | Wazero (pure Go, no CGo) | Production-ready; no native dependencies |
| Extension format | WASM component model + WIT | Stable ABI; language-agnostic; sandboxed |
| HTTP | `chi` + `net/http` (REST + SSE) | Curl-debuggable; SSE for streaming; browser-compatible |
| SQL layer | `sqlc` | Type-safe, generated from SQL; no ORM magic |
| Default storage | `modernc/sqlite` (pure Go) | Zero-ops; no CGo; embedded |
| Storage extensions | `store-postgres.wasm`, `store-supabase.wasm` | Postgres for self-hosted; Supabase for managed cloud |
| UI | Native Go extensions (`ui-tui`, `ui-web`, `ui-gui`) implementing `UIProvider` interface; compiled into binary; selected by CLI flag | WASM sandbox cannot access terminal/window; native Go is the exception for OS-level access |
| API | Native Go extensions (`api-rest`, `api-grpc`, `api-graphql`); pluggable, not built into core | Exposes agent-manager to external consumers; user picks API surface in config |
| Chat integrations | Native Go extensions (`chat-slack`, `chat-telegram`, `chat-whatsapp`, `chat-mattermost`); long-lived connections to messaging platforms | Same agent and memory regardless of channel; configured in jan-klod.yaml |
| Core delivery | Go module (library); binary assembled by thin `cmd/jan-klod/main.go` | Core rarely changes; native extensions change more often; WASM extensions update without recompile |
| Observability | Structured logging + Prometheus + OpenTelemetry | Bundled in core |
| Launcher | Core Go binary itself | No separate supervisor needed |
| Build | Go modules (no Gradle/Maven) | Go toolchain is the build system |
| Linting | `golangci-lint` | User preference |
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

## Open questions (carried forward, still unresolved)

1. ~~Can multiple `llm-provider` extensions be active simultaneously, or only one?~~ **Resolved:** multiple active; `manager-agent-loop` selects per-request.
2. ~~Does `llm-provider` expose a streaming function, or is streaming a capability flag?~~ **Resolved:** streaming is first-class and mandatory; no synchronous path.
3. ~~Do extensions version independently, or does a Jan-Klod release version all together?~~ **Resolved:** independent versioning.
4. ~~How does `registry-mcp` handle MCP server restarts / crashes?~~ **Resolved:** mark server down, remove its tools from active set, emit event to UI, reconnect on exponential backoff. No crash propagates to core.
5. ~~Config hot-reload: can extensions pick up YAML changes without restart?~~ **Resolved:** yes; core watches config file, notifies extensions via event bus.
6. ~~Configurator hosting: self-hosted only, or a public `start.janklod.dev`?~~ **Resolved:** public; GitHub Pages first, `start.janklod.dev` later.
7. ~~Target deployment environment?~~ **Resolved:** desktop (macOS/Windows/Linux), ARM home server/NAS, Docker, Kubernetes.
8. ~~ACP — separate category or under `provider-*`?~~ **Resolved:** new `agent-*` category; `agent-delegate` WIT interface; jan-klod speaks ACP as client and server.

---

## Honest caveats

- `wit-bindgen-go` (WIT bindings for Go) is ~1 year behind the Rust equivalent in maturity. Core WASM loading via Wazero is solid; the WIT component model layer in Go is newer.
- Kotlin/WASM WASI is not production-ready. Extensions must be written in Rust, Go, C, or JS today. Kotlin extension authoring is a future possibility.
- The 15–20MB Go RSS vs 8MB Rust RSS gap is real but irrelevant for a network-bound agent runtime waiting on LLM responses.

---

## Suggested next steps

1. **WIT contract design** — write `.wit` files for the six core interfaces; resolve open questions 1–3 first as they shape signatures directly.
2. **Go project scaffold** — `go mod init`, directory layout, `wazero` dependency, `chi` HTTP skeleton.
3. **First extension** — `store-sqlite.wasm` in Rust (most mature WASM target) to validate the host/guest WIT roundtrip.

---

## Notes for next agent

- Do not reintroduce Java, Maven, or Quarkus. The stack decision is locked.
- Do not add CGo dependencies to the core — keep it pure Go.
- YAML config format is locked. Do not suggest JSON or TOML alternatives.
- Group ID `com.github.janklod` is obsolete — do not reference it.
- UI components (TUI, GUI) are separate binaries talking REST, not WASM extensions.
- Read [BRAINSTORM.md](../2026-06-16-jan-klod/BRAINSTORM.md) for the full original design context before making architecture decisions.
