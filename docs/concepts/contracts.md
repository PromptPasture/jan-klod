---
type: concept
title: Contracts
description: Stable WIT interfaces that form the boundary between core and extensions
tags: [contracts, wit, interfaces, extensions, wasm]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T00:00:00Z
---

Contracts are the stable interfaces that the core exposes and extensions consume or implement. They are the API surface that must not break — a breaking change here breaks all extensions.

Jan-Klod has two classes of contract:

- **WIT interfaces** — for WASM extensions (sandboxed, language-agnostic)
- **Native Go interfaces** — for extensions that need OS access and cannot run in the WASM sandbox (UI only)

## WIT interface overview

| WIT interface | Responsibility | Implemented by |
|---|---|---|
| `llm-provider` | Send prompts; return token streams (streaming is mandatory) | `provider-*` extensions |
| `context-manager` | Manage conversation history; compress as context grows | `manager-context` |
| `agent-manager` | Drive the agent loop (router → step controller → LLM → tool → answer) | `manager-agent-loop` |
| `memory-store` | Persistent key-value or vector store for long-term memory | `store-*` extensions |
| `skill-registry` | Register and resolve reusable agent skills | `registry-skills` |
| `mcp-registry` | Manage MCP server connections and tool discovery | `registry-mcp` |
| `agent-delegate` | Delegate a full task to another AI agent via ACP; receive structured result | `agent-*` extensions |

## Streaming

Streaming is first-class and mandatory in `llm-provider`. There is no synchronous completion path — providers that don't natively stream return a single-token stream. This ensures consistent UX (no blank-screen waits) across local models (llama.cpp, MLX, Ollama) and cloud APIs (OpenAI, Claude).

```wit
interface llm-provider {
    record completion-request {
        messages: list<message>,
        tools: list<tool-definition>,
        max-tokens: u32,
        grammar: option<string>,   // constrained decoding schema
    }

    complete: func(req: completion-request) -> stream<completion-chunk>;
}
```

## Multi-provider

Multiple `llm-provider` extensions can be active simultaneously. `manager-agent-loop` selects the provider per-request based on routing rules in `jan-klod.yaml` (e.g. route code tasks to `provider-ollama`, reasoning to `provider-anthropic`).

## ACP — agent delegation

`agent-*` extensions implement `agent-delegate`. From `manager-agent-loop`'s perspective, delegating to another agent looks like calling a tool — but the sub-agent runs its own full loop and returns a structured result. Jan-Klod also exposes itself as an ACP server, allowing other orchestrators to call it.

## WIT world structure

Each extension declares a WIT world — what it imports from the host and what it exports:

```wit
package jan-klod:contracts;

// Example: an LLM provider extension
world llm-provider-extension {
    import jan-klod:host/http-client;   // host-granted outbound HTTP
    import jan-klod:host/logging;       // host-granted logging
    import jan-klod:host/config;        // own config section (read-only)

    export jan-klod:contracts/llm-provider;  // what this extension provides
}
```

## Core design rules

- Core only depends on WIT interfaces, never on extension implementations.
- Extensions declare which interfaces they require and which they provide.
- Optional dependencies must degrade gracefully (feature off, not crash).
- The host validates the dependency graph at boot and refuses to start with unsatisfied hard dependencies.

## Extension lifecycle (WIT)

Every extension exposes a standard lifecycle interface:

```wit
interface extension-lifecycle {
    init: func(ctx: extension-context) -> result<_, string>;
    start: func() -> result<_, string>;
    stop: func();
    health: func() -> health-status;
}

enum health-status { up, degraded, down }
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

## Status

WIT interface signatures are **not yet written**. The decisions that were blocking this work are now resolved (multi-provider, streaming, independent versioning). The natural next step is writing the `.wit` files and generating host/guest bindings via `wit-bindgen-go`.

See [decisions/2026-06-28-go-wasm-stack/Handoff.md](../decisions/2026-06-28-go-wasm-stack/Handoff.md) for the full stack decision record.
