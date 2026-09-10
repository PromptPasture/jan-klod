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
— which is what a non-Rust client generates its types from. Each value
serializes to the `method`/`params` pair; the `jsonrpc` module wraps that pair
in the JSON-RPC 2.0 frame the transports send.

**The framing moved into the contract in 13b, and that was a correction.** 13a
put it in the transport, reasoning that framing is a transport's business. That
holds for one transport and fails for two parties: the core writes frames and
every client reads them, and `jan-klod` — the TUI client — deliberately depends
on neither `jan-klod-core` nor Wasmtime. A frame type reachable only from the
core would have been hand-rolled a second time in the client, which is the
divergence this contract exists to prevent. What stayed in the transport is what
is genuinely its own: the pipes, the sockets, the read loop. The schema export
describes both — `Command`/`Notification` are the contract, the `jsonrpc.*`
definitions are what goes over the wire.

### Transports

| Transport | How a client reaches it | Built |
|---|---|---|
| stdio JSON-RPC | spawn `jan-klod-gateway rpc`; newline-delimited frames on its stdin/stdout | 13b |
| REST + SSE | `POST /session/:id/message` against a running `serve`, streamed back as `event:`/`data:` | since Phase 3, now a projection |
| WebSocket | — | 13c |

**stdio is the default for `jan-klod`.** No port, no token, nothing left
running: the client spawns the gateway and owns the process. **Stdout carries
frames and nothing else** — every log line, the gateway's own and its guests',
goes to stderr, because a client splitting the stream on newlines would read a
stray `println!` as a frame.

Three things differ between the two, and they are consequences of the shape
rather than choices:

- **A confirmation** is answered on a second connection over REST; over stdio
  there is one pipe, so a reader thread holds it while the turn runs and hands
  frames to the loop between the turn's own events.
- **A cancel** over REST is the client disconnecting — there is no route for it.
  Over stdio `turn/cancel` is a frame, read at the same seam, and it cancels by
  the same mechanism a disconnect does (the event sink returning `Stop`).
- **Steering** (`turn/follow-up`) works over stdio and cannot over REST, which
  has no way to deliver a message into a turn already running.

An answer that arrives when nothing asked is refused on both — `409` over REST,
`invalid request` over stdio. Not stashed: a held answer would sit until the
*next* question and approve it, which is how "yes" to reading a file becomes
"yes" to running a command.

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

`jan_klod_protocol::compatible` is what decides, and **while the version is
`0.x` a differing minor is refused as well** — the same rule the WIT
`api-version` follows for the same reason (see [Versioning](#versioning)
below): every version in play is `0.x`, so a major-only check would wave a `0.9`
client through to a `0.1` core and call that a negotiation. The two predicates
are deliberately separate. The version lines are independent — a WIT change need
not touch a command, and a new command need not touch WIT — so one function
serving both would mean one line dragging the other to a decision it did not
make. A refused handshake carries the core's own version in the error's `data`,
because it is the one exchange that returns no `HelloResult` to read it from.

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

## Versioning

`jan-klod:interfaces` is versioned as an ABI, because that is what it is: an
extension is compiled against it and the host cannot recompile one.

**Where the version lives.** `package jan-klod:interfaces@X.Y.Z` at the top of
every `wit/*.wit`, and every file must agree. Two versions leave no answer to
which one the host speaks, so both readers of that number refuse rather than
pick one: `scripts/manifests.sh` fails if `wit/` declares more than one, and
`host/tests/it/manifest.rs::the_hosts_api_version_matches_the_wit_package`
asserts the host's `core::manifest::API_VERSION` equals it. The host holds a
constant rather than reading `wit/` because an installed gateway has no `wit/`
beside it; that test is the price of the constant.

**What counts as which bump.**

| Change to `wit/` | Bump |
|---|---|
| A function or interface removed or renamed | **major** |
| A function's parameters or result re-typed | **major** |
| A record field removed, renamed, or re-typed | **major** |
| A case added to an `enum` or `variant` | **major** |
| A field added to an existing record | **major** |
| A new function, interface, or record | **minor** |
| Comments, doc text, formatting | **patch** |

**Two rows look additive and are not**, which is the reason for a table rather
than the sentence "additive is minor". The component model types records,
enums and variants **structurally**: a record with one more field is a different
type, not a compatible extension of the old one, so a guest built against the
old shape cannot link against a host exporting the new one. The same goes for a
case added to an `enum` or `variant` — and there a guest matching exhaustively
over the old set does not cover the new case either. So both are major, by the
test that defines major: an existing component cannot survive it.

There is no such thing as an optional field to add. A field is part of the
shape; optionality lives in its *type* (`option<T>`), which has to be there from
the start to help.

**Pre-1.0 is stricter than semver-by-habit suggests.** While the major is `0`
the package is free to change, so a `0.x` version carries no compatibility
promise — and *because* it carries none, a differing **minor** is refused. Read
quickly, semver says `0.1` and `0.9` differ only in a minor and might be
compatible; here they are not, and the host says so.
`core::manifest::api_compatible` implements exactly this: same major, and the
same minor while the major is `0`. From `1.0` on, a differing minor passes,
which is what a minor bump means. A version that does not parse is
incompatible — guessing is how a check becomes decoration.

**What the host does with it today.** Every component ships an
`api-version` in its [manifest](#the-extension-manifest), and `Runtime::boot`
refuses an incompatible one, naming both versions and the component
(`host/tests/it/manifest.rs::a_component_built_against_another_api_version_is_refused`).
That is the whole of it, and two things it deliberately does **not** do are
worth naming so nobody builds against them:

- **There are no adapters.** An incompatible version is refused, not bridged.
  Keeping N-1 minor compatibility by adapting — the Zed model — is
  [slice 16b-3](https://github.com/PromptPasture/jan-klod/issues/90), deferred
  until there is a version pair it would help; below `1.0` there is none.
- **The version travels one way.** It reaches the host through the manifest.
  `extension-lifecycle`'s `extension-context` carries the extension's *own*
  build version (`version: string`), not the interface package's, so a guest
  cannot currently read what the host speaks and adapt to it. That direction is
  also #90.

**The freeze.** Until the first public release these rules describe intent and
`wit/` may still change freely; from that release they bind, and the
version-bump check ([16b-2](https://github.com/PromptPasture/jan-klod/issues/89))
becomes a failure rather than a warning. See the
[roadmap](roadmap.md#phase-16--capability-manifest--signed-registry).

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
