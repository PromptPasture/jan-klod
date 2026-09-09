---
type: concept
title: Contracts
description: Stable WIT interfaces that form the boundary between core and extensions
tags: [contracts, wit, interfaces, extensions, wasm]
created: 2026-06-28T00:00:00Z
updated: 2026-09-09T00:00:00Z
---

Contracts are the stable interfaces that the core exposes and extensions consume or implement. They are the API surface that must not break — a breaking change here breaks all extensions.

Every extension is a sandboxed WASM component, so **WIT interfaces are the only
extension contract** — there is no native/in-core extension tier. (UIs are not
extensions; they connect to core over its host-side client surface — see
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
`on-error`, `finalize`, `prepare-next-turn`) — a new lifecycle point is a new enum case, never a new
function. **Ordering is structural, not configured:** across phases it follows the enum;
within a phase, deterministic extension load order. `config.yaml` **only enables/disables**
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
| `host-config.wit` | `host-config` | Read own section of `config.yaml` |
| `host-event.wit` | `host-event` | Event bus publish/subscribe — **observation-only** (fire-and-forget); cannot shape the loop |
| `host-storage.wit` | `host-storage` | Namespaced view of the core's own store. **Granted** (`persist: true`), never ambient; namespaces are prefixed with the calling component's id |

Interceptor dispatch is **not** in this table: it is a core-native call of the
extension-exported `interceptor` interface (above), not a capability extensions
import.

`host-serve` and `host-socket` are **planned** capabilities: they are what keep
`api-*` (inbound listeners) and `chat-*` (long-lived connections) fully
sandboxed instead of needing raw OS access. Today's `host-http` is
outbound-request-only and does not cover either case.

Two further substrate capabilities are **not yet scoped**: **`host-fs`** (scoped
workspace-filesystem access) for file-touching tools (read/write, edit, grep/find,
git), and **`host-process`** (spawn/hold a long-lived child process) for execution
tools — **code execution (`bash`/`eval`)**, ssh, and the LSP/DAP/browser bridges. Both
are needed because the sandbox denies raw filesystem and process access by design.
Recorded as the [file-workspace tier](roadmap.md#file-workspace-tier-not-yet-scoped);
no contract is defined until that tier is scoped.

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

Multiple `llm-provider` extensions can be active simultaneously. `interceptor-task-router` selects the provider per-request (at the `select-model` phase) based on routing rules in `config.yaml` (e.g. route code tasks to `provider-ollama`, reasoning to `provider-anthropic`).

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

There is **no WIT UI contract.** UIs are not extensions and run in their own
processes; they reach core the same way any external client does — over the
core's host-side client surface, the LSP/server model. Today that surface is the
REST + SSE API described in [Architecture → Transport](architecture.md#transport);
it is built into the core binary since Phase 3, so the earlier question of a
separate `api-rest` guest is closed.

**Phase 13 — the client protocol is a contract of the same rank as WIT.** The
`jan-klod-protocol` crate (`src/core/protocol`) holds the typed commands and
notifications, `PROTOCOL_VERSION`, and a JSON Schema export in
[`schema/protocol.schema.json`](../../src/core/protocol/schema/protocol.schema.json)
— which is what a non-Rust client generates its types from. The crate carries no
transport and no request ids: each value serializes to the `method`/`params`
pair, and the envelope around it belongs to whichever transport carries it
(13b stdio, 13c WebSocket). Landed in 13a; the transports have not.

**Commands** (client → core):

| Command | Params | REST route today |
|---|---|---|
| `protocol/hello` | `version` | — (negotiation is new) |
| `session/create` | — | `POST /sessions` |
| `session/list` | — | `GET /sessions` |
| `session/get` | `session` | `GET /session/:id` |
| `session/message` | `session`, `message` | `POST /session/:id/message` |
| `turn/answer` | `session`, `answer` | `POST /session/:id/answer` |
| `turn/cancel` | `session` | — (today: drop the SSE connection) |
| `turn/follow-up` | `session`, `message` | — (no steering over REST) |

**Notifications** (core → client). The first five are `conductor::Event` one for
one; `ask` is a turn blocked on the user, `error` a failed turn or an unservable
command, `session/updated` a transcript that moved:

| Notification | Params | SSE frame today |
|---|---|---|
| `text-delta` | `text` | `delta` |
| `tool-invoked` | `id`, `name`, `arguments` | `tool` (drops `arguments`) |
| `tool-result` | `id`, `content` | `tool-result` |
| `warning` | `message` | `warning` |
| `done` | `answer`, `agentic` | `done` |
| `ask` | `session`, `question`, `options`, `default` | `prompt` |
| `error` | `message` | `error` (payload key `error`) |
| `session/updated` | `session`, `preview` | — |

REST + SSE is a **projection** of this, not a second contract, and it keeps its
own older spellings — `delta`, `tool`, `prompt` — deliberately. Neither side is
being renamed to match: `core/tests/protocol_events.rs` asserts every key an SSE
frame carries reaches the notification with an equal value, which is what holds
the two together. The projection may lose nothing; it may lag in naming.

**Version rule.** `PROTOCOL_VERSION` is semver. Removing a command, renaming
one, or removing a field bumps **major**; adding a command or an optional field
bumps **minor**. A client sends the version it was built against in
`protocol/hello` and the core answers with its own, so a mismatch surfaces at
connect rather than mid-turn. The schema export carries the version too, so a
bump cannot land without the schema being regenerated.

**The open question the vision left — own schema, or ACP wholesale — is settled
as: own schema.** ACP becomes an *adapter* over this contract in Phase 18, not
the internal representation. The reason is ownership of the compatibility story:
an editor protocol we do not control would decide when our own clients break,
and three of the surfaces that must share this format (TUI, web, scripts) are
not editors at all. An adapter costs one translation layer in one phase; adopting
a foreign schema costs a veto over every future change. See the
[vision](../decisions/2026-09-08-harness-platform-vision/Vision.md#decisions) and
the [roadmap](roadmap.md#phase-13--client-protocol).

## The extension manifest

A contract that travels *with* a component, rather than one it implements. The
WIT interfaces on this page say what a component may be asked to do; the
manifest says what it needs in order to do it, in a form a registry can read
before anything is downloaded and a host can check before anything runs.

`ext/<name>.manifest.toml`, beside the `.wasm` — a sidecar rather than a custom
wasm section, so it is inspectable without a wasm parser:

```toml
name = "tool-shell"
version = "0.1.0"
api-version = "0.1.0"
kind = "tool"
description = "run a command through host-process"
capabilities = [
    "host-process",
]
```

**`capabilities` is read from the component, not written by its author.** The
generator (`scripts/manifests.sh`, run by `make -C src/extensions manifests`)
takes the top-level world's `import` lines out of `wasm-tools component wit` and
keeps the `host-*` interfaces. So a manifest cannot claim less than the artifact
beside it does, and two things it would be easy to wrongly include are excluded
deliberately:

- **Exports are not capabilities.** `tool-callable` and `extension-lifecycle`
  are what a guest *implements*. A reading that took every `jan-klod:interfaces`
  mention in the WIT output would list them.
- **Type-only imports are not capabilities.** `llm-types` and `store-types` are
  shapes; nothing is granted by importing one, and listing them would tell an
  operator to allow `llm-types`, which means nothing.

An empty list is written as `capabilities = []` rather than omitted: "needs
nothing" is a claim worth making, and a missing key reads as unfilled.

**The host reads it at boot, and refuses three things.** A component whose
manifest omits a capability it imports; a component with no manifest at all,
unless top-level `allow-unmanifested: true` says otherwise; and a component
built against an incompatible `jan-klod:interfaces` version. Each refusal names
the component and what is wrong with it — the interface that is undeclared, the
grant that would permit an unmanifested load, or both versions.

Two things it deliberately does **not** refuse. Declaring a capability
`config.yaml` does not grant is fine and grants nothing, because every
capability is default-deny where it is used; refusing it would make the manifest
a second place grants must be kept in step with, so an author would have to
track every operator's config. And a differing *minor* version from `1.0` on
passes, since that is what a minor bump means — though while the package is
`0.x` a differing minor is refused, because a pre-release version carries no
promise at all.

What a manifest does not do is decide what a component may *do*. That is still
`config.yaml`'s grants, each default-deny, unchanged by anything declared here.
A manifest makes a component's needs **inspectable before it runs** and its
description **checkable against itself**; it is not a permission.

## Versioning (planned, Phase 16)

The `jan-klod:interfaces` package is free to change until the first public
release. From then on it follows semver: a **major** bump for any change an
existing component cannot survive (removed or re-typed function, changed record
field), a **minor** bump for additive change. Every extension carries the
`api-version` it was built against (in `extension-lifecycle` and in its
manifest); the host refuses an incompatible major with a clear error and keeps
**N-1 minor** compatibility through adapters rather than breaking releases — the
Zed model. Plan in the [roadmap](roadmap.md#phase-16--capability-manifest--signed-registry).

## Storage is not a contract extensions implement

There was a `memory-store.wit` here, and a `store-*` component family in the
architecture notes. One component was ever written against it (`store-memory`),
and the core never called it once: persistence has always been host-side, for the
reason [Architecture](architecture.md#storage) records — the sandbox has no
filesystem, so a store guest would need one granted back, and the transcript is
the most sensitive thing the runtime holds. The contract and the family are gone;
`host-storage` is how a guest reaches storage, and the top-level `storage:` block
is how an operator configures it.

See [decisions/2026-06-29-component-model-rust/Handoff.md](../decisions/2026-06-29-component-model-rust/Handoff.md) for the current foundation decision (Rust + Wasmtime + Component Model), which supersedes the host language and runtime of the earlier [2026-06-28 Go + Wazero stack](../decisions/2026-06-28-go-wasm-stack/Handoff.md). The WIT contracts on this page are unchanged by that pivot.
