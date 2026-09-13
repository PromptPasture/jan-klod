---
type: concept
title: Contracts
description: Stable WIT interfaces that form the boundary between core and extensions
tags: [contracts, wit, interfaces, extensions, wasm]
created: 2026-06-28T00:00:00Z
updated: 2026-09-09T00:00:00Z
---

Contracts are stable interfaces the core exposes and extensions consume or implement. A breaking change breaks all extensions.

Every extension is a sandboxed WASM component, so **WIT interfaces are the only contract** — no native tier. UIs aren't extensions; they connect over the client surface — see [UI ↔ core](#ui--core-client-surface).

## WIT interface overview

All interfaces live in `wit/` under `jan-klod:interfaces@0.1.0`, validated with `wasm-tools component wit wit/`.

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

**Interceptor dispatch is core-native.** The core loop invokes enabled `interceptor-*` components directly and acts on their returned `decision` (`proceed`/`replace`/`block`/`ask`). One generic function over a `phase` enum (`session-start`, `before-loop`, `select-model`, `select-context`, `select-tools`, `after-response`, `tool-call`, `tool-result`, `on-error`, `finalize`, `prepare-next-turn`) — new lifecycle points are new enum cases, never new functions. **Ordering is structural:** across phases it follows the enum; within a phase it's deterministic extension load order. `config.yaml` **only enables/disables** — interceptors declare phases via `subscribed-phases()`. `intercept` returns `result<decision, interceptor-error>`; errors/traps fail closed at `tool-call`, open-with-log elsewhere.

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

Interceptor dispatch isn't in this table: it's a core-native call of `interceptor` (above), not a capability.

`host-serve` and `host-socket` are **planned** — they keep `api-*` and `chat-*` sandboxed instead of needing raw OS access. `host-http` today is outbound-only.

**Not yet scoped**: `host-fs` (file tools) and `host-process` (long-lived children). Both needed because the sandbox denies raw access. See [file-workspace tier](roadmap.md#file-workspace-tier-not-yet-scoped).

## Streaming

Streaming is first-class and mandatory in `llm-provider` — no synchronous path. Non-streaming providers return single-token streams, ensuring consistent UX (no blank-screen waits) across local models and cloud APIs.

Streaming uses **poll-based handles** rather than native WIT `stream<>` (forced by early `wit-bindgen-go` immaturity). `complete` returns an opaque `stream-handle`; the host polls `next-chunk` until `done`, then calls `close-stream`. Same pattern used by `agent-delegate` (`delegate-handle`) and the core-exposed loop entry drivers call (`run-handle`).

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

Multiple `llm-provider` extensions can run simultaneously. `interceptor-task-router` selects per-request (at `select-model`) based on `config.yaml` routing rules (e.g. code → `provider-ollama`, reasoning → `provider-anthropic`).

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

- Core depends only on WIT interfaces.
- Extensions declare required and provided interfaces.
- Optional dependencies degrade gracefully.
- Host validates the dependency graph at boot.

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

There is **no WIT UI contract.** UIs are not extensions and run in their own processes; they reach core the same way any external client does — over the core's host-side client surface. Today that surface is the REST + SSE API; it is built into the core binary since Phase 3.

**Phase 13 — the client protocol is a contract of the same rank as WIT.** The `jan-klod-protocol` crate (`src/protocol`) holds the typed commands and notifications, `PROTOCOL_VERSION`, and a JSON Schema export in [`schema/protocol.schema.json`](../../src/protocol/schema/protocol.schema.json) — which is what a non-Rust client generates its types from.

### Transports

| Transport | How a client reaches it | Built |
|---|---|---|
| stdio JSON-RPC | spawn `jan-klod-gateway rpc`; newline-delimited frames on its stdin/stdout | 13b |
| REST + SSE | `POST /session/:id/message` against a running `serve`, streamed back as `event:`/`data:` | since Phase 3, now a projection |
| WebSocket | — | 13c |

**stdio is the default for `jan-klod`.** No port, no token, nothing left running: client spawns the gateway and owns it. **Stdout carries frames only** — all logs go to stderr, so clients splitting on newlines don't read stray `println!` as frames.

Three differences follow from the shape:

- **Confirmation**: answered on a second REST connection; over stdio one pipe, a reader thread holds it while the turn runs and hands frames between events.
- **Cancel**: REST is client disconnect (no route); stdio is `turn/cancel` frame read at the same seam.
- **Steering** (`turn/follow-up`): works over stdio, not REST, which can't deliver messages into running turns.

An answer that arrives when nothing asked is refused on both — `409` over REST, `invalid request` over stdio. Not stashed: a held answer would sit until the *next* question and approve it.

**Commands** (client → core):

| Command | Params | REST route today |
|---|---|---|
| `protocol/hello` | `version` | — (negotiation is new) |
| `session/create` | — | `POST /sessions` |
| `session/list` | — | `GET /sessions` |
| `session/get` | `session` | `GET /session/:id` |
| `session/fork` | `session`, `at-seq` | `POST /session/:id/fork` |
| `session/message` | `session`, `message` | `POST /session/:id/message` |
| `turn/answer` | `session`, `answer` | `POST /session/:id/answer` |
| `turn/cancel` | `session` | — (today: drop the SSE connection) |
| `turn/follow-up` | `session`, `message` | — (no steering over REST) |

**Notifications** (core → client). The first five are `conductor::Event` one for one; `ask` is a turn blocked on the user, `error` a failed turn or an unservable command, `session/updated` a transcript that moved:

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

REST + SSE is a **projection** of this, not a second contract, and it keeps its own older spellings — `delta`, `tool`, `prompt` — deliberately. Neither side is being renamed to match: `core/tests/protocol_events.rs` asserts every key an SSE frame carries reaches the notification with an equal value, which is what holds the two together.

**Declared results**: `protocol/hello` answers `HelloResult`; `session/get` answers `SessionGetResult` — `{ id, messages }`, each message `{ seq, role, content, tool-call-id? }`.

`seq` is the log position and `session/fork`'s `at-seq` parameter — inclusive, so forking at a message's seq yields a session whose transcript ends with that message. **It is not an index**: events that project to no message (an ask, an answer, a text delta) still consume a position, so the numbers are sparse and the gaps are not loss.

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

**Version rule.** `PROTOCOL_VERSION` is semver: remove/rename command or field → **major**; add command/optional field → **minor**. Clients send built-against version in `protocol/hello`, core answers, mismatch surfaces at connect. Schema export carries version too; bumps need schema regeneration.

While version is `0.x` a differing minor is refused too — same WIT rule: all versions are `0.x`, so major-only checks would pass `0.9` to `0.1` calling it negotiation.

## The extension manifest

A contract that travels *with* a component, rather than one it implements. The WIT interfaces on this page say what a component may be asked to do; the manifest says what it needs in order to do it, in a form a registry can read before anything is downloaded and a host can check before anything runs.

`ext/<name>.manifest.toml`, beside the `.wasm` — a sidecar rather than a custom wasm section, so it is inspectable without a wasm parser:

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

**`capabilities` is read from the component, not written by its author.** The generator (`scripts/manifests.sh`, run by `make -C src/extensions manifests`) takes the top-level world's `import` lines out of `wasm-tools component wit` and keeps the `host-*` interfaces. So a manifest cannot claim less than the artifact beside it does.

An empty list is written as `capabilities = []` rather than omitted: "needs nothing" is a claim worth making, and a missing key reads as unfilled.

**The host reads it at boot, and refuses three things.** A component whose manifest omits a capability it imports; a component with no manifest at all, unless top-level `allow-unmanifested: true` says otherwise; and a component built against an incompatible `jan-klod:interfaces` version. Each refusal names the component and what is wrong with it.

Two things it deliberately does **not** refuse. Declaring a capability `config.yaml` does not grant is fine and grants nothing, because every capability is default-deny where it is used. And a differing *minor* version from `1.0` on passes, since that is what a minor bump means — though while the package is `0.x` a differing minor is refused.

Manifests don't grant permissions — `config.yaml`'s grants do.

## Versioning

`jan-klod:interfaces` is versioned as an ABI, because that is what it is: an extension is compiled against it and the host cannot recompile one.

**Where the version lives.** `package jan-klod:interfaces@X.Y.Z` at the top of every `wit/*.wit`, and every file must agree. Two versions leave no answer to which one the host speaks, so both readers of that number refuse rather than pick one: `scripts/manifests.sh` fails if `wit/` declares more than one. The host holds a constant rather than reading `wit/` because an installed gateway has no `wit/` beside it.

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

**Two rows look additive and are not.** The component model types records, enums and variants **structurally**: a record with one more field is a different type, not a compatible extension of the old one, so a guest built against the old shape cannot link against a host exporting the new one. The same goes for a case added to an `enum` or `variant`.

There is no such thing as an optional field to add. A field is part of the shape; optionality lives in its *type* (`option<T>`), which has to be there from the start to help.

**Pre-1.0 is stricter than typical semver.** While major is `0` the package is free to change, so `0.x` carries no promise — differing **minor** is refused. Semver says `0.1` and `0.9` might be compatible; here they're not. `core::manifest::api_compatible` requires same major and same minor while major is `0`; from `1.0` on, differing minor passes.

**What the host does with it today.** Every component ships an `api-version` in its [manifest](#the-extension-manifest), and `Runtime::boot` refuses an incompatible one, naming both versions and the component. That is the whole of it.

There are no adapters. An incompatible version is refused, not bridged. The version travels one way: it reaches the host through the manifest. `extension-lifecycle`'s `extension-context` carries the extension's *own* build version (`version: string`), not the interface package's, so a guest cannot currently read what the host speaks and adapt to it.

**The freeze**: Until first release, rules describe intent; from then, they bind. See [roadmap](roadmap.md#phase-16--capability-manifest--signed-registry).

## Signed artefacts and the registry index

**Planned, Phase 16.** Written before the slices that implement it so the layout is written down first. Where a slice finds this page wrong, the slice changes the page in the same commit.

### What travels together

A component is three files with one stem, beside each other wherever they are — a registry directory, a release archive, or `ext/`:

| File | What it is | Who produces it |
|---|---|---|
| `<name>.wasm` | the component | the build |
| `<name>.manifest.toml` | the [manifest](#the-extension-manifest), plus `sha256 = "<hex>"` of the `.wasm` beside it | `scripts/manifests.sh` |
| `<name>.manifest.toml.minisig` | a [minisign](https://jedisct1.github.io/minisign/) signature **over the manifest** | the release workflow |

**The signature is over the manifest, and the manifest carries the hash of the component.** That is one signature covering both files without inventing a container format: the signature proves who wrote the manifest, the manifest's `sha256` proves which bytes it describes. This layout is checkable by hand with the stock `minisign` binary and `sha256sum`, which is the test a signature format has to pass.

`sha256` is a generated field: the generator computes it from the staged `.wasm`, so a manifest cannot describe a component other than the one beside it. The host does not check it at boot — a file already in `ext/` is trusted the way `config.yaml` is. It is checked at **install**, which is the moment bytes cross from untrusted to trusted.

### Keys

Minisign keys. The public key is the one-line format `minisign` itself writes and is named in `config.yaml` as a grant:

```yaml
registry:
  trusted-keys:
    - name: jan-klod-release
      key: "RWQ…"        # the base64 line of the .pub file, inline
```

The key is inline rather than a path so a config is self-contained and a `${VAR}` cannot swap it. **With no `trusted-keys`, every install is refused as unsigned** unless the installer is told, per install, to allow it; that flag is the narrow widening.

### What install verifies, in order

Nothing half-verified is ever visible in `ext/`: everything below happens in a staging directory, and the last step is one atomic rename. Each refusal has its **own** message naming what is wrong.

1. **Checksum**, when the caller supplied one, against the `.wasm`. Optional for a local path — the bytes are already on the machine and hashing them with a value computed from the same file checks nothing. **Required for a URL**, with no flag to skip it: a signature proves the publisher, not that this is the version the user meant, and the value pasted from a release page is the only thing tying the download to the intent.
2. **Signature** of `<name>.manifest.toml` against `registry.trusted-keys`, then the manifest's `sha256` against the `.wasm`. Refused when the key is valid but not trusted, when the signature is valid but the hash is not, and when there is no signature — the last one unless explicitly allowed.
3. **It is a component.** `Component::from_file` succeeds; a core module or arbitrary bytes are refused here, not later at boot.
4. **The manifest agrees with the component**: the same import cross-check boot performs, run once more on the staged copy, so a signed manifest that lies about its own component is refused before it lands.
5. **Rename into `ext/`.** A failure at any earlier step removes the staging directory and leaves `ext/` byte-identical.

### The index

A registry is a directory served over HTTPS with an `index.toml` at its root — TOML like the manifests, so the one parser already in the tree reads both, and a file a person can read without tooling:

```toml
# index.toml
format = 1

[[extension]]
name = "tool-shell"
version = "0.1.0"
api-version = "0.1.0"
kind = "tool"
description = "run a command through host-process"
capabilities = ["host-process"]
sha256 = "9f2c…"                         # of tool-shell.wasm; equals the manifest's
path = "tool-shell/0.1.0/tool-shell.wasm" # relative to the index; siblings by stem
signed-by = "jan-klod-release"           # a key name a config can be expected to hold
```

- **Every per-extension field is copied from the manifest**, so an index entry can be checked against the manifest it points at, and `ext search` can show requested capabilities *before* a byte is downloaded — which is the reason the index has them. `format` is the index's own version, bumped when the shape changes; a reader refuses a `format` it does not know rather than guessing.
- **`path` names the `.wasm`; the manifest and signature are its siblings by stem.** One field, not three, so the three files cannot be listed in three places and disagree. Relative to the index URL, so a registry can be moved or mirrored by copying a directory.
- **The index is signed too**: `index.toml.minisig`, by the same key, verified on fetch when a trusted key exists. It is served over TLS regardless; the signature is what stops a substituted index from pointing every entry at an older, still-signed version.
- **One index, one publisher.** A `signed-by` other than a configured key is an entry the installer cannot use and says so; federating indexes is not designed here.

What this is not: a package manager. No dependency resolution, no version ranges, no update channel — an entry is an exact artefact, and installing it is the five steps above. See [roadmap](roadmap.md#phase-16--capability-manifest--signed-registry).

## Storage is not a contract extensions implement

There was a `memory-store.wit` here, and a `store-*` component family in the architecture notes. One component was ever written against it (`store-memory`), and the core never called it once: persistence has always been host-side, for the reason [Architecture](architecture.md#storage) records — the sandbox has no filesystem, so a store guest would need one granted back, and the transcript is the most sensitive thing the runtime holds. The contract and the family are gone; `host-storage` is how a guest reaches storage, and the top-level `storage:` block is how an operator configures it.

See [decisions/2026-06-29-component-model-rust/Handoff.md](../decisions/2026-06-29-component-model-rust/Handoff.md) for the current foundation decision (Rust + Wasmtime + Component Model), which supersedes the host language and runtime of the earlier [2026-06-28 Go + Wazero stack](../decisions/2026-06-28-go-wasm-stack/Handoff.md). The WIT contracts on this page are unchanged by that pivot.
