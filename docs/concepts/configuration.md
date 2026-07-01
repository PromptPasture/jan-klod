---
type: concept
title: Configuration
description: The jan-klod.yaml format and how the core loads it into extension instances
tags: [config, yaml, extensions, host-config, loader]
created: 2026-06-29T00:00:00Z
updated: 2026-06-29T00:00:00Z
---

A single `jan-klod.yaml` declares which extensions run and how they are
configured. The **core** loads it (`src/core/config/`, the `jan-klod-config`
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

## Inspecting a config

`make config` resolves the repo's `jan-klod.yaml` and prints the plan (each
instance → its wasm), which is also how the loader is exercised:

```
make config
# [enabled ] provider.openai        -> provider-openai.wasm
# [disabled] provider.lm-studio     -> provider-openai.wasm
# …
```

See [Architecture](architecture.md) for the extension taxonomy and
[Contracts](contracts.md) for the `host-config` interface the sections are
served through.
