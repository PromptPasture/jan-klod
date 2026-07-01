---
type: concept
title: Contracts
description: Stable WIT interfaces that form the boundary between core and extensions
tags: [contracts, wit, interfaces, extensions, wasm]
created: 2026-06-28T00:00:00Z
updated: 2026-07-01T00:00:00Z
---

Contracts are the stable interfaces that the core exposes and extensions consume or implement. They are the API surface that must not break — a breaking change here breaks all extensions.

Every extension is a sandboxed WASM component, so **WIT interfaces are the only
extension contract** — there is no native/in-core extension tier. (UIs are not
extensions; they connect to core over an `api-*` network surface — see
[UI ↔ core](#ui--core-client-surface) below.)

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
| `interceptor.wit` | `interceptor` | Agent-loop decision hook — one generic `intercept` over a `phase` enum; the core loop calls it per phase and acts on its `proceed` / `replace` / `block` / `ask` return | `interceptor-*` |
| `memory-store.wit` | `memory-store` | Persistent key-value storage | `store-*` |
| `skill-registry.wit` | `skill-registry` | Skill catalog and dispatch | `registry-skills` |
| `mcp-registry.wit` | `mcp-registry` | MCP server management + tool catalog | `registry-mcp` |
| `agent-delegate.wit` | `agent-delegate` | ACP agent delegation (streaming) | `agent-*` |
| `tool-callable.wit` | `tool-callable` | Discrete callable tool | `tool-*` |

**Interceptor dispatch is core-native.** The core loop invokes each enabled
`interceptor-*` component's exported `interceptor` interface directly and acts on the
returned `decision` (`proceed` / `replace` / `block` / `ask`). There is **no `host-hook`
capability an extension imports** — dispatch is a synchronous, ordered call driven by the
host, distinct from the observation-only `host-event` bus. The interface is **one generic
function over a `phase` enum** (`session-start`, `before-loop`, `select-model`,
`select-context`, `select-tools`, `after-response`, `tool-call`, `tool-result`,
`finalize`, `prepare-next-turn`) — a new lifecycle point is a new enum case, never a new
function. **Ordering is structural, not configured:** across phases it follows the enum;
within a phase, deterministic extension load order. `jan-klod.yaml` **only enables/disables**
interceptors — an interceptor declares the phases it wants via `subscribed-phases()`. The
`ask` decision routes a question through the loop to the attached driver (which prompts in
its own idiom) and resumes on the answer, so a rule-based permission gate can confirm with
the user without touching a UI. `intercept` returns `result<decision, interceptor-error>`;
on error or trap the host **fails closed at `tool-call`** and fails-open-with-log elsewhere.

> **`context-manager` is subsumed into `interceptor-context`.** History trimming and
> compression are no longer a loop-facing contract the host routes to; under the
> generic-hook model the loop only knows the `interceptor` interface, and
> `interceptor-context` performs history/compression *internally* at the
> `select-context` phase. `context-manager.wit` is retained (if at all) only as an
> internal type source, not a routed interface.

### Host-provided interfaces

Core grants these capabilities to every extension.

| File | Interface | Purpose |
|---|---|---|
| `host-http.wit` | `host-http` | Outbound HTTP **requests** (outbound-request access) |
| `host-serve.wit` | `host-serve` | **Inbound** listener — lets `api-*` bind a port and serve REST/gRPC *(planned)* |
| `host-socket.wit` | `host-socket` | Long-lived bidirectional socket — lets `chat-*` hold a Telegram/Slack connection *(planned)* |
| `host-log.wit` | `host-log` | Structured logging forwarded to core pipeline |
| `host-config.wit` | `host-config` | Read own section of `jan-klod.yaml` |
| `host-event.wit` | `host-event` | Event bus publish/subscribe — **observation-only** (fire-and-forget); cannot shape the loop |
| `host-storage.wit` | `host-storage` | Proxy to active `memory-store` (subset: no purge/search) |

Interceptor dispatch is **not** in this table: it is a core-native call of the
extension-exported `interceptor` interface (above), not a capability extensions
import.

`host-serve` and `host-socket` are **planned** capabilities: they are what keep
`api-*` (inbound listeners) and `chat-*` (long-lived connections) fully
sandboxed instead of needing raw OS access. Today's `host-http` is
outbound-request-only and does not cover either case.

## Streaming

Streaming is first-class and mandatory in `llm-provider`. There is no synchronous completion path — providers that don't natively stream return a single-token stream. This ensures consistent UX (no blank-screen waits) across local models (llama.cpp, MLX, Ollama) and cloud APIs (OpenAI, Claude).

Streaming uses a **poll-based handle** model rather than native WIT `stream<>`. (This was forced by `wit-bindgen-go` immaturity in the Go MVP; under Wasmtime + `wit-bindgen` the native `stream<>`/async path should be re-evaluated, but the poll-based handle remains the safe default until proven.) `complete` returns an opaque `stream-handle`; the host polls `next-chunk` until it yields `done`, then calls `close-stream`. The same pattern is used by `agent-delegate` (`delegate-handle`) and by the **core-exposed loop entry** that drivers (`api-*`/`chat-*`) call to run the agent (`run-handle`).

> **`agent-manager` is retired as an extension interface.** Its `run` /
> `next-event` / `cancel` / `close` surface described the old
> `manager-agent-loop` extension. The loop is now core *mechanism*
> ([Architecture](architecture.md#agent-loop-architecture)), so that surface — plus
> steering / follow-up injection and a tool-result `terminate` — moves to a
> **core-exposed driver interface** *(planned)*, and the decision logic it used to
> hold moves to `interceptor-*` extensions. `agent-manager.wit` is superseded.

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

Multiple `llm-provider` extensions can be active simultaneously. `interceptor-task-router` selects the provider per-request (at the `select-model` phase) based on routing rules in `jan-klod.yaml` (e.g. route code tasks to `provider-ollama`, reasoning to `provider-anthropic`).

## ACP — agent delegation

`agent-*` extensions implement `agent-delegate`. From the core loop's perspective, delegating to another agent looks like calling a tool — but the sub-agent runs its own full loop and returns a structured result. Jan-Klod also exposes itself as an ACP server, allowing other orchestrators to call it.

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

## UI ↔ core (client surface)

There is **no native UI contract.** UIs are not extensions and run in their own
processes; they reach core the same way any external client does — over an
`api-*` surface (REST + SSE), the LSP/server model. The shape of that surface is
the `api-*` extension's published API (e.g. `api-rest`'s HTTP routes + SSE event
stream), not a WIT extension boundary and not a Rust trait baked into core.

*(Open: whether core also exposes a minimal built-in local control endpoint so a
UI client can attach to a bare core with no `api-*` enabled. Current lean: a UI
deployment includes `api-rest`.)*

## MemoryStore implementations

Three planned `memory-store` implementations — see [Architecture](architecture.md#storage) for the comparison table.

See [decisions/2026-06-29-component-model-rust/Handoff.md](../decisions/2026-06-29-component-model-rust/Handoff.md) for the current foundation decision (Rust + Wasmtime + Component Model), which supersedes the host language and runtime of the earlier [2026-06-28 Go + Wazero stack](../decisions/2026-06-28-go-wasm-stack/Handoff.md). The WIT contracts on this page are unchanged by that pivot.
