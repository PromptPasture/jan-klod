---
type: concept
title: Configuration
description: The config.yaml format and how the core loads it into extension instances
tags: [config, yaml, extensions, host-config, loader]
created: 2026-06-29T00:00:00Z
updated: 2026-09-09T00:00:00Z
---

`config.yaml` declares which extensions run and how. The **core** loads it and turns it into **extension instances**; everything domain-specific is opaque and handed to the instance through [`host-config`](contracts.md#host-provided-interfaces).

## File shape

Extensions are grouped by **category** (`provider`, `store`, `interceptor`, `registry`, `tool`, `agent`, `api`, `chat`). Each *named* entry is one **instance**:

```yaml
extensions:
  provider:
    openai:                       # instance name
      enabled: true
      type: openai                # wasm discriminator (optional)
      base-url: https://api.openai.com/v1
      api-key: ${OPENAI_API_KEY}  # opaque to the core
      model: gpt-4o-mini
    lm-studio:
      enabled: false
      type: openai                # reuses provider-openai.wasm
      base-url: http://localhost:1234/v1
```

## What the core reads

The core interprets exactly two keys per entry; **everything else is opaque**.

| Key | Meaning | Default |
|---|---|---|
| `enabled` | Whether to load the instance | `false` |
| `type` | Wasm discriminator within the category | the entry name |

**Component resolution:** `ext/<category>-<type>.wasm`. Since `type` defaults to entry name, `provider.openai` → `provider-openai.wasm`. Multiple instances can share one `type` — every OpenAI-compatible endpoint runs on `provider-openai.wasm`; give an instance its own `type` for different payloads/headers (e.g. `anthropic` → `provider-anthropic.wasm`).

**Instance identity:** `<category>.<name>` is the unique id. Instances can share components but keep separate config sections.

## What is in `ext/`

The directory the core resolves components against — **build output**: `make ext` puts `.wasm` per guest, plus `<name>.manifest.toml` declaring what it needs. Neither is tracked in git; `make -C src/extensions clean` removes both. `ext/` is a directory one command produces, not a place to keep things.

A manifest is generated from the component's imports, describes the `.wasm`, and cannot drift; see [Contracts → The extension manifest](contracts.md#the-extension-manifest). Reading one answers "what does this want?" without a WASM parser:

```console
$ cat ext/tool-shell.manifest.toml | grep -A2 capabilities
capabilities = [
    "host-process",
]
```

**The host checks it at boot**: components without a manifest, ones that under-declare imports, or those built against incompatible `jan-klod:interfaces` are refused, not loaded. See [Contracts → The extension manifest](contracts.md#the-extension-manifest) for format and exceptions.

| Key | Meaning | Default |
|---|---|---|
| `allow-unmanifested` | Load a component that ships no manifest beside it. **Top-level**, not under `extensions:` — every key there is a category of named instances | `false` |

Off by default because inspectable components are what manifests enforce. `make ext` generates one per guest and `make bundle` carries them into releases, so the only way to skip this is external components — exactly when you want to be asked. The grant waives *declaration*, not inspection: imports are always read. Manifests don't decide what a component may *do*; that's this file's grants, each default-deny.

### Putting something in `ext/` (`registry:`)

`make ext` fills `ext/` from the repo. External components go in with `jan-klod-gateway ext install`, which verifies before copying — the build pipeline isn't sandboxed, so `ext/` contents decide what runs with your privileges. `cp` performs no checks.

| Key | Meaning | Default |
|---|---|---|
| `registry.trusted-keys` | Minisign public keys that may vouch for an installed component. **Top-level**, and distinct from `extensions.registry`, which is the category holding the `skills` and `mcp` catalogues — they share a word and nothing else | `[]` |

**Empty list means nothing is trusted, not "skip the check"** — the same default-deny rule: named no keys = granted nothing, every install refused until configured. Empty meaning unsigned-is-fine would disappear the check for exactly the person who never configured it.

A signature covers **both** component and manifest. Manifests carry no provenance; the host trusts them at boot purely for sitting beside the component. A signature over the `.wasm` alone would verify the artefact while trusting someone else's description of what it may ask for.

Nothing first-party is signed yet, so installs need the deliberate widening:

```console
$ jan-klod-gateway ext install ./tool-thing.wasm --sha256 <hex> --allow-unsigned
installed tool-thing into ext/ — may use host-fs
```

`--allow-unsigned` **requires** `--sha256`, so no combination lands a component with nothing vouching for it. Digests matter when they reached you by a different route than the bytes — a release note, not a hash of the file you're installing.

A refused install changes nothing: the pair is assembled in `ext/.staging/`
and `ext/` is left byte-identical. `ext list` shows what is there with the
capabilities each component declares, and `ext remove` takes a component and
its manifest together.

**URLs work like paths**, same checks apply:

```console
$ jan-klod-gateway ext install https://example.com/ext/tool-thing.wasm
```

The URL names the **component**; manifest and signatures fetch from beside it (`tool-thing.manifest.toml`, `tool-thing.wasm.minisig`, `tool-thing.manifest.toml.minisig`) — same layout as disk, so publishers serve one directory and both forms work. With `--allow-unsigned` signatures aren't requested.

Remote installs refuse two things local ones can't:

- **Denied destinations** — public addresses only; loopback, private, link-local, and cloud-metadata refused *before a byte moves*, every redirect re-checked. Private sources can't get in today; that grant arrives when something needs it.
- **URLs with query/fragment** — companions are derived by resolving relative references, which drops query — so `…/tool-thing.wasm?token=abc` fetches the component authenticated but the manifest not, 404 reads as "no manifest". Refusing states what's wrong.

## MCP servers (`extensions.registry.mcp`)

`registry-mcp` connects to MCP servers and offers their tools namespaced by server — a `tools/list` entry `echo` on server `fixture` reaches the model as `fixture::echo`. Each `servers` entry names one server and how to reach it:

```yaml
extensions:
  registry:
    mcp:
      enabled: true
      servers:
        - name: remote-docs          # over HTTP
          transport: sse
          url: https://mcp.example.com/rpc
        - name: local-docs           # over a stdio child
          transport: stdio
          child: docs-mcp            # optional; defaults to `name`
```

| Key | Meaning |
|---|---|
| `name` | What the server is called, and the namespace its tools appear under |
| `transport` | `sse`, `streamable-http`, or `stdio` |
| `url` | Where to reach it. HTTP transports only |
| `child` | Which `execution.long-lived` entry to start. `stdio` only; defaults to `name` |

### `transport: stdio`

Most MCP servers are stdio processes, not HTTP endpoints. A `stdio` entry doesn't name a command — it names a child granted in [`execution.long-lived`](#executionlong-lived), and the host supplies the program:

```yaml
execution:
  enabled: true
  long-lived:
    - name: docs-mcp
      command: /usr/bin/my-mcp-server
      args: ["--stdio"]
```

A server gets written twice — once as runnable, once as reachable — and that's the point: extensions can't introduce programs, only ask for ones you named.

Three things worth knowing:

- **Stdio children are confined by [`execution.sandbox`](#executionsandbox) like any command, so no network by default.** MCP servers reaching APIs will fail looking broken. If it needs the network, say so in the sandbox policy — note that's a decision about what a third-party program may do on your machine.
- **It's killed when the instance goes**, including on gateway exit. No orphaned servers to clean up or reuse.
- **Servers that never answer are given up on after ten seconds**, reported as down. Turns still run; tools are absent. Same for servers that answer non-JSON.

## Opaque config and `${VAR}` expansion

Every key except `enabled`/`type` is the instance's private config section. The core doesn't interpret it — it env-expands `${VAR}` and serves the section back through `host-config` (as JSON).

`${VAR}` expands from the process environment. **Unset variables are hard errors**, but **only for enabled instances** — disabled providers may reference missing secrets without blocking startup.

## Core invariants

The loader enforces core-level rules only; domain rules live in consuming extensions.

- **At most one `store.*` enabled** — core proxies `host-storage` to one active store.

`providers` and `routing` (fallback chain and task→model routing) are **top-level, not under `extensions`**. Core preserves them for `interceptor-task-router` (via `host-config`) without validating references — routing is interceptor domain logic. See [Architecture → Provider fallback](architecture.md#provider-fallback) and [Task routing](architecture.md#task-routing).

## Command execution (`execution:`)

A top-level block, not under `extensions` — it's a substrate for enabled tools, like `workspace:` backs `host-fs`. Default-deny: no `execution:` block means `host-process` refuses all calls, and `tool.shell`/`tool.git` load but can't run.

| Key | Meaning | Default |
|---|---|---|
| `enabled` | Whether `host-process` runs commands at all. Also requires a workspace, since the cwd is jailed to it | `false` |
| `timeout-secs` | A command outrunning this is killed | `30` |
| `output-cap` | Captured bytes per stream, stdout and stderr each. Also bounds what a **long-lived** child may hold unread: past it the oldest output is dropped and the host says so, because a child nobody reads would otherwise grow the host for as long as it runs | `65536` |
| `env-passthrough` | Extra environment names a child inherits, one at a time. Everything else is stripped — the gateway's own environment holds your API keys | none |
| `sandbox` | What a command may do once running (below) | see below |
| `long-lived` | Children a guest may hold open, **named one at a time** (below) | none |

### `execution.long-lived`

Processes a guest may start and keep running — what stdio MCP servers, `lsp-*`, or browser drivers need; `enabled` alone can't express this because `host-process`'s `exec` runs to completion.

```yaml
execution:
  enabled: true
  long-lived:
    - name: docs-mcp            # what a guest asks for
      command: /usr/bin/my-mcp  # what actually runs
      args: ["--stdio"]
```

**Narrower than `enabled`, not wider.** A guest names a child; command and args come from here. Granted guests can start only what you wrote — no path lets a guest-chosen string become a program. `enabled: true` grants none alone: empty or absent list means no child starts.

Missing `name` or `command` is skipped, not guessed. Half-written grants are unreadable; conservative reading is none.

Long-lived children are confined by the same `execution.sandbox` policy as one-shot commands — they outlive their call, so more exposed — and killed when the instance goes, including on gateway exit. Guests that never call `kill` can't leave processes behind.

A guest can ask which names it was granted (`host-process.granted`), names only — what a name runs never leaves the host. That is what lets a *tool* offer them: see `tool.proc` below.

#### Letting the model drive one (`tool.proc`)

The grants above are reachable only by a guest. `tool.proc` is the shipped tool that puts them in front of the model — `list`, `start`, `output`, `stop`, each naming a child from the list above:

```yaml
extensions:
  tool:
    proc:
      enabled: true
```

Two grants, not one: enabling the tool grants nothing on its own, and an empty `long-lived` list leaves it with nothing to start. The model never supplies a command, so the widest thing it can do is start something you wrote down — the same rule that makes this section narrower than `enabled`, unchanged by having a model on the other end of it.

### `execution.sandbox`

Everything above bounds the **caller**: jailed cwd, rebuilt environment, time/output caps. None bounds the **command**, which runs with your privileges and can read/write anywhere you can. `sandbox` is the policy; mode is how the runtime tells you whether anything enforces it.

| Key | Meaning | Default |
|---|---|---|
| `mode` | `os` asks the operating system to confine the command; `approval-only` says nothing does | `os` |
| `writable` | Workspace-relative roots a command may write to. An entry outside the workspace is refused | `["."]` |
| `network` | Whether a command may reach the network | `false` |
| `require` | `true` denies `host-process` entirely rather than falling back to `approval-only` — no command at all, in preference to an unconfined one | `false` |

**macOS and Linux have a backend** — Seatbelt via `sandbox-exec`, Landlock via a ruleset (Phase 15b/15c) — so `writable` and `network` are enforced there. Every other platform is `approval-only`: the confirmation prompt is the only barrier and those two keys have no effect. Boot prints the effective mode and why when it's not what you asked:

```console
WARN [core] `execution.sandbox.mode: os` was requested, but this build has no
sandbox backend for macos — a command is confined only by the confirmation prompt
```

Two paths are granted to every confined command whatever `writable` says, because a command that cannot use them does not run at all: `/dev/null`, and `TMPDIR`, which the host points at `<workspace>/.jan-klod/tmp` rather than inheriting. Both have a row in [the security model](security-model.md).

Two configurations refuse rather than degrade, both reported at boot: `require: true` without a backend, and an unreadable `sandbox` block (unrecognised `mode`, or `writable` leaving workspace). Both deny `host-process` — an unhonoured policy must not read as a grant.

## Interceptors

Interceptors (`interceptor.*` category) are extensions with an `enabled` flag. Config **only enables/disables** them — never orders them. Dispatch order is **structural**: across phases by `phase` enum order, within a phase by extension **load order** (see [`wit/interceptor.wit`](contracts.md) and [Roadmap → Phase 2](roadmap.md#phase-2--first-real-value-the-agent-loop)). No config key sequences steps, so ordering can't drift.

```yaml
extensions:
  interceptor:
    intent-router:      # before-loop: simple vs agentic classification
      enabled: true
    permission:         # tool-call: confirm anything not known read-only
      enabled: false
```

The `permission` interceptor enables/disables like any other, but its policy is also tunable. `safe-calls` **replaces** the built-in allowlist (doesn't extend it); scope checks toggle independently. Omit a key to keep defaults:

```yaml
    permission:
      enabled: true
      safe-calls: [find, fs:read, fs:grep, git, edit:view, proc-probe]
      allow-absolute-paths: false     # true = absolute-path args skip the scope gate
      allow-parent-traversal: false   # true = `..` traversal skips the scope gate
```

The gate is an **allowlist**: unnamed calls run only after user confirmation. Previous denylist was high-risk verbs, naming only what someone thought of — `tool-edit`'s ops are `view`, `replace`, `insert`, no `write`, so the file-modifying tool was ungated from its commit. Allowlists gate unclassified by construction. Cost: add a tool and it prompts until listed.

Entries are `name` (every op) or `name:op`. `git` is listed bare because its op
set is closed and read-only; `fs` is listed per op because it can also write.
`fetch` is deliberately absent — it is network egress, which is worth a question
even though it does not touch the workspace.

### `guardrails`

Where `permission` asks whether an action may happen, `guardrails` looks at what
the text says — tool arguments, the model's output, the assembled answer, and
the messages heading for the provider. **Off by default**: a content filter
nobody asked for is a surprise, and with no rules it changes nothing.

Two optional lists, both under the extension's own key:

```yaml
    guardrails:
      enabled: true
      deny-tool-arguments:              # matched against a call's arguments
        - pattern: "rm +-rf +/"         # required
          tool: shell                   # optional; every tool when absent
          reason: "a recursive delete"  # optional; shown to whoever is refused
          decision: block               # optional; `block` or `ask`, default `block`
      redact:                           # matched against model output, the final
        - pattern: "sk-[A-Za-z0-9]{16,}"  # answer, and outbound messages
          with: "[redacted]"            # optional; this is the default
          decision: replace             # optional; `replace` or `block`, default `replace`
```

Rules are **data** — patterns and a decision each, never code. The engine is the
`regex` crate, which matches in linear time and cannot backtrack: matching sits
on the path of every tool call, so a pattern that could be made to hang would be
a denial-of-service surface inside the thing meant to prevent them. Lookaround
and backreferences are the price, and a pattern using them is refused when the
rules load rather than silently never matching.

A malformed rule invalidates the **whole** set; every dispatch then reports an
internal error and the host fails closed at `tool-call`. A typo stops tool calls
until it is fixed rather than leaving a guardrail that is silently absent.

Full rule reference, including how the lists compose:
[`src/extensions/interceptor-guardrails/README.md`](../../src/extensions/interceptor-guardrails/README.md).

## Inspecting a config

`make config` resolves the repo's `config.yaml` and prints the plan (instance → wasm), exercising the loader:

```
make config
# [enabled ] provider.openai        -> provider-openai.wasm
# [disabled] provider.lm-studio     -> provider-openai.wasm
# …
```

## Per-project configuration

Jan-klod reads two things from the **workspace root**:

```
AGENTS.md                — project instructions, appended to system prompt
.agents/skills/          — project-specific skills, loaded by registry-skills
```

`AGENTS.md` holds conventions otherwise repeated each session: test commands, what to avoid, codebase terminology. Read host-side and handed to `interceptor-system` as config — not by granting filesystem access, since guests need one file's contents, not file-opening ability.

It's appended to the standing prompt and **labelled as the project's own**, with stated authority: repo instructions shape the agent but can't grant sandbox-denied permissions. They need telling apart — otherwise "you may write anywhere" reads to the model as a runtime fact. Setting `prompt: ""` switches both off; half an explicit "no" is worse.

Sent on every turn, capped at 16 kB and truncated rather than silently halved.

**Workspace root only** — not nearest ancestor. Earlier versions promised ancestor search; climbing above the root is exactly what path jails prevent, and repos checked out inside other projects would silently inherit instructions. This page also mentioned a `session-start` interceptor phase (gone): both files are read at agent build.

See [Architecture](architecture.md) for the extension taxonomy and
[Contracts](contracts.md) for the `host-config` interface the sections are
served through.
