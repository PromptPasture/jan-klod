---
type: concept
title: Contracts
description: Stable WIT interfaces that form the boundary between core and extensions
tags: [contracts, wit, interfaces, extensions, wasm]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T15:00:00Z
---

Contracts are the stable interfaces that the core exposes and extensions consume or implement. They are the API surface that must not break — a breaking change here breaks all extensions.

Jan-Klod has two classes of contract:

- **WIT interfaces** — for WASM extensions (sandboxed, language-agnostic)
- **Native Go interfaces** — for extensions that need OS access and cannot run in the WASM sandbox (UI only)

## WIT interface overview

All interfaces live in `wit/` under the package `jan-klod:interfaces@0.1.0`. The
package is validated with `wasm-tools component wit wit/`.

### Shared interfaces

Type-only and lifecycle interfaces consumed across the package.

| File | Interface | Purpose |
|---|---|---|
| `types.wit` | `llm-types`, `store-types` | Single canonical source for cross-interface records (`message`, `role`, `tool-call`, `entry`, …) so consumers version independently |
| `extension-lifecycle.wit` | `extension-lifecycle` | `init` / `start` / `stop` / `health` — exported by **every** extension world so the host manages them uniformly |

### Extension-exported interfaces

Extensions implement these and the host routes calls between them.

| File | Interface | Responsibility | Implemented by |
|---|---|---|---|
| `llm-provider.wit` | `llm-provider` | Streaming completions, constrained decoding | `provider-*` |
| `context-manager.wit` | `context-manager` | Conversation history + compression | `manager-context` |
| `agent-manager.wit` | `agent-manager` | Agent loop, routing, decomposition, fallback | `manager-agent-loop` |
| `memory-store.wit` | `memory-store` | Persistent key-value storage | `store-*` |
| `skill-registry.wit` | `skill-registry` | Skill catalog and dispatch | `registry-skills` |
| `mcp-registry.wit` | `mcp-registry` | MCP server management + tool catalog | `registry-mcp` |
| `agent-delegate.wit` | `agent-delegate` | ACP agent delegation (streaming) | `agent-*` |
| `tool-callable.wit` | `tool-callable` | Discrete callable tool | `tool-*` |

### Host-provided interfaces

Core grants these capabilities to every extension.

| File | Interface | Purpose |
|---|---|---|
| `host-http.wit` | `host-http` | Outbound HTTP (only network access extensions have) |
| `host-log.wit` | `host-log` | Structured logging forwarded to core pipeline |
| `host-config.wit` | `host-config` | Read own section of `jan-klod.yaml` |
| `host-event.wit` | `host-event` | Event bus publish/subscribe |
| `host-storage.wit` | `host-storage` | Proxy to active `memory-store` (subset: no purge/search) |

## Streaming

Streaming is first-class and mandatory in `llm-provider`. There is no synchronous completion path — providers that don't natively stream return a single-token stream. This ensures consistent UX (no blank-screen waits) across local models (llama.cpp, MLX, Ollama) and cloud APIs (OpenAI, Claude).

Streaming uses a **poll-based handle** model rather than native WIT `stream<>`, which is not yet mature in Wazero / wit-bindgen-go. `complete` returns an opaque `stream-handle`; the host polls `next-chunk` until it yields `done`, then calls `close-stream`. The same pattern is used by `agent-manager` (`run-handle`) and `agent-delegate` (`delegate-handle`).

```wit
interface llm-provider {
    use llm-types.{role, message, tool-definition, tool-call};

    type stream-handle = u32;

    variant completion-chunk {
        text-delta(string),
        tool-call-request(tool-call),
        done(string),              // reason: "stop" | "tool-calls" | "length" | "error"
    }

    record completion-request {
        model: string,
        messages: list<message>,
        tools: list<tool-definition>,
        grammar: option<string>,   // constrained decoding schema
        max-tokens: option<u32>,
        temperature: option<f32>,
    }

    complete: func(request: completion-request) -> result<stream-handle, provider-error>;
    next-chunk: func(handle: stream-handle) -> option<completion-chunk>;
    close-stream: func(handle: stream-handle);
}
```

## Multi-provider

Multiple `llm-provider` extensions can be active simultaneously. `manager-agent-loop` selects the provider per-request based on routing rules in `jan-klod.yaml` (e.g. route code tasks to `provider-ollama`, reasoning to `provider-anthropic`).

## ACP — agent delegation

`agent-*` extensions implement `agent-delegate`. From `manager-agent-loop`'s perspective, delegating to another agent looks like calling a tool — but the sub-agent runs its own full loop and returns a structured result. Jan-Klod also exposes itself as an ACP server, allowing other orchestrators to call it.

## WIT world structure

Each `.wit` file defines a `world` — what the extension imports from the host and what it exports. Example from `llm-provider.wit`:

```wit
world provider-world {
    import host-log;
    import host-config;
    import host-http;

    export extension-lifecycle;
    export llm-provider;
}
```

Same-package references use the short, unversioned form (`import host-log;`); the fully-qualified `jan-klod:interfaces/host-log@0.1.0` form would make the package depend on itself and fail validation.

The full world for each extension type is in its respective `.wit` file.

## Core design rules

- Core only depends on WIT interfaces, never on extension implementations.
- Extensions declare which interfaces they require and which they provide.
- Optional dependencies must degrade gracefully (feature off, not crash).
- The host validates the dependency graph at boot and refuses to start with unsatisfied hard dependencies.

## Extension lifecycle (WIT)

Every extension world exports `extension-lifecycle` (`extension-lifecycle.wit`) so the host can initialise, start, stop, and health-check any extension uniformly:

```wit
interface extension-lifecycle {
    enum health-status { up, degraded, down }

    record extension-context {
        id: string,       // e.g. "provider-openai"
        version: string,  // semver of this build
    }

    init: func(ctx: extension-context) -> result<_, string>;
    start: func() -> result<_, string>;
    stop: func();
    health: func() -> health-status;
}
```

## Native Go interface

`UIProvider` is the only native Go contract. It is implemented by `ui-tui`, `ui-web`, and `ui-gui` — all compiled into the binary.

```go
type UIProvider interface {
    Start(ctx context.Context, api AgentAPI) error
    Stop() error
    Health() HealthStatus
}
```

Native extensions follow the same lifecycle and config conventions as WASM extensions. They are declared in `jan-klod.yaml` under `extensions:` and selected at runtime via CLI flag.

## MemoryStore implementations

Three planned `memory-store` implementations — see [Architecture](architecture.md#storage) for the comparison table.

See [decisions/2026-06-28-go-wasm-stack/Handoff.md](../decisions/2026-06-28-go-wasm-stack/Handoff.md) for the full stack decision record.
