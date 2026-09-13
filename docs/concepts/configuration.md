---
type: concept
title: Configuration
description: The config.yaml format and how the core loads it into extension instances
tags: [config, yaml, extensions, host-config, loader]
created: 2026-06-29T00:00:00Z
updated: 2026-09-09T00:00:00Z
---

A single `config.yaml` declares which extensions run and how they are
configured. The **core** loads it (`src/config/`, the `jan-klod-config`
crate) and turns it into a list of **extension instances**; everything domain-
specific is opaque to the core and handed to the instance through
[`host-config`](contracts.md#host-provided-interfaces).

## File shape

Extensions are grouped by **category** (`provider`, `store`, `interceptor`,
`registry`, `tool`, `agent`, `api`, `chat`). Each *named* entry under a category
is one extension **instance**:

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

**Component resolution:** the wasm is `ext/<category>-<type>.wasm`. Because
`type` defaults to the entry name, `provider.openai` resolves to
`provider-openai.wasm`. Point several instances at the same `type` to **reuse
one component** — every OpenAI-compatible endpoint (`openai`, `lm-studio`,
`groq`, …) runs on `provider-openai.wasm`; give an instance its own `type` when
it needs different payloads/headers (e.g. `anthropic` → `provider-anthropic.wasm`).

**Instance identity:** `<category>.<name>` (e.g. `provider.openai`) is the unique
id. Several instances can share one component but each keeps its own config
section and identity.

## What is in `ext/`

The directory the core resolves components against, and it is **build output**:
`make ext` puts a `.wasm` there per enabled-able guest, plus a
`<name>.manifest.toml` declaring what that component needs. Neither is tracked
in git — `make -C src/extensions clean` removes both — so `ext/` is a directory
one command produces whole rather than a place to keep things.

A manifest is generated from the component's own imports, so it describes the
`.wasm` beside it and cannot drift from it; the format is in
[Contracts → The extension manifest](contracts.md#the-extension-manifest).
Reading one is how you answer "what does this thing want?" without a wasm
parser:

```console
$ cat ext/tool-shell.manifest.toml | grep -A2 capabilities
capabilities = [
    "host-process",
]
```

**The host checks it at boot**, so a component that ships without one, or with
one that under-declares what it imports, or built against an incompatible
`jan-klod:interfaces` version, is refused with a reason rather than loaded and
trusted. The format and what is *not* refused are in
[Contracts → The extension manifest](contracts.md#the-extension-manifest).

| Key | Meaning | Default |
|---|---|---|
| `allow-unmanifested` | Load a component that ships no manifest beside it. **Top-level**, not under `extensions:` — every key there is a category of named instances | `false` |

Off by default, because a component nobody can inspect before running it is the
thing a manifest exists to prevent. `make ext` generates one per guest and
`make bundle` carries them into a release, so the only way to meet this refusal
is a component from somewhere else — which is exactly when you want to be
asked. The grant waives the *declaration*, not the inspection: what a component
imports is still read either way.

What a manifest still does not decide is what a component may *do*. That
remains this file's grants, each default-deny.

### Putting something in `ext/` (`registry:`)

`make ext` fills `ext/` from this repository. A component from anywhere else
goes in with `jan-klod-gateway ext install`, which verifies before it copies —
because the build pipeline is not behind the runtime sandbox, so whatever lands
in `ext/` decides what runs with your privileges. `cp` performs no checks at
all.

| Key | Meaning | Default |
|---|---|---|
| `registry.trusted-keys` | Minisign public keys that may vouch for an installed component. **Top-level**, and distinct from `extensions.registry`, which is the category holding the `skills` and `mcp` catalogues — they share a word and nothing else | `[]` |

**An empty list means nothing is trusted, not "skip the check".** That is the
same default-deny rule as every other grant here: an operator who has named no
keys has granted nothing, so every install is refused until a key is
configured. The alternative reading — empty means unsigned is fine — would make
the check disappear for exactly the person who never configured it.

A signature must cover **both** the component and its manifest. A manifest
carries no provenance of its own; the host trusts it at boot purely for sitting
beside the component, so a signature over the `.wasm` alone would verify the
artefact while trusting someone else's description of what it may ask for.

Nothing first-party is signed yet, so today an install needs the deliberate
widening:

```console
$ jan-klod-gateway ext install ./tool-thing.wasm --sha256 <hex> --allow-unsigned
installed tool-thing into ext/ — may use host-fs
```

`--allow-unsigned` **requires** `--sha256`, so there is no combination of flags
that lands a component with nothing vouching for it. A digest is worth having
only when it reached you by a different route than the bytes did — a release
note, not a hash of the file you are about to install.

A refused install changes nothing: the pair is assembled in `ext/.staging/`
and `ext/` is left byte-identical. `ext list` shows what is there with the
capabilities each component declares, and `ext remove` takes a component and
its manifest together.

**A URL works wherever a path does**, and the same checks run on what arrives:

```console
$ jan-klod-gateway ext install https://example.com/ext/tool-thing.wasm
```

The URL names the **component**; the manifest and the signatures are fetched
from beside it (`tool-thing.manifest.toml`, `tool-thing.wasm.minisig`,
`tool-thing.manifest.toml.minisig`) — the same layout as on disk, so a
publisher serves one directory and both forms work. With `--allow-unsigned` the
signatures are not requested at all.

Two things a remote install refuses that a local one cannot:

- **A destination the egress policy denies** — public addresses only, so
  loopback, private, link-local and cloud-metadata URLs are refused *before a
  byte moves*, and every redirect hop is re-checked rather than followed on
  trust. A component source on a private address therefore has no way in today;
  that grant arrives with the registry when something needs it.
- **A URL carrying a query or fragment.** The companion URLs are derived by
  resolving a relative reference, which drops a query — so
  `…/tool-thing.wasm?token=abc` would fetch the component authenticated and the
  manifest not, and the resulting 404 would read as "no manifest published".
  Refusing says what is wrong instead.

## MCP servers (`extensions.registry.mcp`)

`registry-mcp` connects to MCP servers and offers their tools to the agent,
namespaced by server — a `tools/list` entry called `echo` on a server called
`fixture` reaches the model as `fixture::echo`.

Each entry under `servers` names one server and how to reach it:

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

Most MCP servers distributed today are stdio processes rather than HTTP
endpoints. A `stdio` entry does **not** name a command — it names a child the
operator granted in [`execution.long-lived`](#executionlong-lived), and the host
supplies the program:

```yaml
execution:
  enabled: true
  long-lived:
    - name: docs-mcp
      command: /usr/bin/my-mcp-server
      args: ["--stdio"]
```

So a server has to be written down twice — once as something that may run, once
as something to talk to — and that is the point rather than an inconvenience: an
extension cannot introduce a program, only ask for one you already named.

Three things worth knowing before you configure one:

- **A stdio child is confined by [`execution.sandbox`](#executionsandbox) like
  any other command, so by default it has no network.** An MCP server that
  reaches out to an API will fail in a way that looks like the server being
  broken. If it needs the network, say so in the sandbox policy — and note that
  is a decision about what a third-party program may do on your machine, not a
  formality.
- **It is killed when the extension instance goes**, including when the gateway
  exits. There is no orphaned server to clean up, and none to reuse either.
- **A server that never answers is given up on after ten seconds** and reported
  as down. A turn still runs; its tools are simply absent. The same is true of
  one that answers something that is not JSON.

## Opaque config and `${VAR}` expansion

Every key in an entry other than `enabled`/`type` is the instance's private
config section. The core does not interpret it — it env-expands `${VAR}`
references and serves the section back verbatim through `host-config` (as JSON).

`${VAR}` is expanded from the process environment. An **unset** variable is a
hard error, but **only for enabled instances** — a disabled provider may
reference a secret that isn't present, and that must not block startup.

## Core invariants

The loader enforces only core-level rules; domain rules live in the extensions
that consume them.

- **At most one `store.*` may be enabled** — the core proxies `host-storage` to a
  single active store.

`providers` and `routing` (provider fallback chain and task→model routing) are
**top-level, not under `extensions`**. The core preserves them verbatim for the
`interceptor-task-router` extension (via `host-config`) and does **not** validate
their references — routing is interceptor domain logic, not a core concern. See
[Architecture → Provider fallback](architecture.md#provider-fallback) and
[Task routing](architecture.md#task-routing).

## Command execution (`execution:`)

A top-level block, not under `extensions` — it is a substrate the core hands to
whichever tools are enabled, the same way `workspace:` backs `host-fs`.
Default-deny: with no `execution:` block, `host-process` refuses every call, and
`tool.shell` / `tool.git` load but cannot run anything.

| Key | Meaning | Default |
|---|---|---|
| `enabled` | Whether `host-process` runs commands at all. Also requires a workspace, since the cwd is jailed to it | `false` |
| `timeout-secs` | A command outrunning this is killed | `30` |
| `output-cap` | Captured bytes per stream, stdout and stderr each | `65536` |
| `env-passthrough` | Extra environment names a child inherits, one at a time. Everything else is stripped — the gateway's own environment holds your API keys | none |
| `sandbox` | What a command may do once running (below) | see below |
| `long-lived` | Children a guest may hold open, **named one at a time** (below) | none |

### `execution.long-lived`

A list of processes a guest may start and keep running — what a stdio MCP
server, an `lsp-*` or a browser driver needs, and what `enabled` alone cannot
express, because `host-process`'s `exec` runs to completion.

```yaml
execution:
  enabled: true
  long-lived:
    - name: docs-mcp            # what a guest asks for
      command: /usr/bin/my-mcp  # what actually runs
      args: ["--stdio"]
```

**This is narrower than `enabled`, not wider.** A guest names a child; the
command and its arguments come from here. So a granted guest can start what you
wrote down and nothing else — there is no path by which a string the guest chose
becomes a program. `enabled: true` on its own grants none of these: an empty or
absent list means no child can be started.

An entry missing `name` or `command` is skipped rather than guessed. A
half-written grant is one nobody can read, and the conservative reading of it is
none.

A long-lived child is confined by the same `execution.sandbox` policy as a
one-shot command — it outlives its call, so it is more exposed, not less — and
it is killed when the extension instance that started it goes, including when
the gateway exits. A guest that never calls `kill` cannot leave a process
behind.

### `execution.sandbox`

Everything above bounds the **caller**: a jailed cwd, a rebuilt environment,
time and output caps. None of it bounds the **command**, which runs with your
privileges and can read or write anywhere you can. `sandbox` is the policy for
that, and the mode is how the runtime tells you whether anything is enforcing
it.

| Key | Meaning | Default |
|---|---|---|
| `mode` | `os` asks the operating system to confine the command; `approval-only` says nothing does | `os` |
| `writable` | Workspace-relative roots a command may write to. An entry outside the workspace is refused | `["."]` |
| `network` | Whether a command may reach the network | `false` |
| `require` | `true` denies `host-process` entirely rather than falling back to `approval-only` — no command at all, in preference to an unconfined one | `false` |

**No OS backend exists yet** (macOS Seatbelt and Linux Landlock are
[Phase 15b/15c](roadmap.md#phase-15--os-level-effect-sandbox)), so every
platform resolves to `approval-only` today: the confirmation prompt is the only
barrier, and `writable`/`network` have no effect until a backend lands. This is
never quiet about itself — with execution enabled, boot prints the effective
mode and, when it is not the one you asked for, why:

```console
WARN [core] `execution.sandbox.mode: os` was requested, but this build has no
sandbox backend for macos — a command is confined only by the confirmation prompt
```

Two configurations refuse rather than degrade, and both say so at boot:
`require: true` while no backend exists, and a `sandbox` block that cannot be
read at all (an unrecognised `mode`, or a `writable` entry leaving the
workspace). In both cases `host-process` is denied — a policy the runtime cannot
honour must not read as a grant.

## Interceptors

Interceptors (the `interceptor.*` category) are extensions like any other, each
with an `enabled` flag. Config **only enables or disables** an interceptor — it
never orders or sequences them. Dispatch order is **structural**: across phases
by the `phase` enum declaration order, and within a single phase by extension
**load order** (see [`wit/interceptor.wit`](contracts.md) and
[Roadmap → Phase 2](roadmap.md#phase-2--first-real-value-the-agent-loop)). Because
there is no config key that sequences steps, interceptor ordering cannot drift.

```yaml
extensions:
  interceptor:
    intent-router:      # before-loop: simple vs agentic classification
      enabled: true
    permission:         # tool-call: confirm anything not known read-only
      enabled: false
```

The `permission` interceptor is enabled/disabled like any other, but its policy
is also tunable. `safe-calls` **replaces** the built-in allowlist when present
(it does not extend it); scope checks are toggled independently. Omit a key to
keep its default:

```yaml
    permission:
      enabled: true
      safe-calls: [find, fs:read, fs:grep, git, edit:view, proc-probe]
      allow-absolute-paths: false     # true = absolute-path args skip the scope gate
      allow-parent-traversal: false   # true = `..` traversal skips the scope gate
```

The gate is an **allowlist**: a call that is not named runs only after the user
confirms it. This was a denylist of high-risk verbs, which can only name the
verbs someone thought of — `tool-edit`'s ops are `view`, `replace` and `insert`,
none of which is `write`, so the tool whose purpose is modifying files was
ungated from the commit that added it. An allowlist gates the unclassified by
construction. The cost is real: add a tool and it prompts until it is listed.

Entries are `name` (every op) or `name:op`. `git` is listed bare because its op
set is closed and read-only; `fs` is listed per op because it can also write.
`fetch` is deliberately absent — it is network egress, which is worth a question
even though it does not touch the workspace.

## Inspecting a config

`make config` resolves the repo's `config.yaml` and prints the plan (each
instance → its wasm), which is also how the loader is exercised:

```
make config
# [enabled ] provider.openai        -> provider-openai.wasm
# [disabled] provider.lm-studio     -> provider-openai.wasm
# …
```

## Per-project configuration

In addition to `config.yaml`, jan-klod reads two things from the **workspace root**:

```
AGENTS.md                — project instructions, appended to the system prompt
.agents/skills/          — project-specific skills, loaded by registry-skills
```

`AGENTS.md` is where the conventions you would otherwise repeat every session
live: which test command to run, what not to touch, how this codebase spells
things. It is read host-side and handed to `interceptor-system` as config — not by
granting interceptors filesystem access, because the guest needs one file's
contents rather than the ability to open files.

It is appended to the standing prompt and **labelled as the project's own**, with
its authority stated: instructions from a repository can shape how the agent works,
and cannot grant permissions the sandbox refuses. The two need telling apart —
otherwise "you may write anywhere" in a checked-in file reads to the model as a
fact about the runtime. Setting `prompt: ""` switches both off; honouring half of
an explicit "no system message" would be worse than either answer.

Sent on every turn, so it is capped at 16 kB and truncated with a note rather than
silently halved.

**The workspace root only** — not the nearest ancestor. An earlier version of this
page promised ancestor search; climbing above the root is exactly what the path
jail exists to prevent, and a repository checked out inside another project would
silently inherit its instructions. This page also described a `session-start`
interceptor phase, which no longer exists: both files are read when the agent is
built.

See [Architecture](architecture.md) for the extension taxonomy and
[Contracts](contracts.md) for the `host-config` interface the sections are
served through.
