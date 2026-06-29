---
type: concept
title: Blue/Green Deployment
description: Zero-downtime update strategy for Jan-Klod runtime and extensions
tags: [deployment, updates, rollback]
created: 2026-06-28T00:00:00Z
updated: 2026-06-29T00:00:00Z
---

Jan-Klod uses a blue/green strategy to update the runtime or extensions without downtime and with instant rollback.

A **small standalone supervisor** (planned: a **TinyGo** binary) performs the
update: it stages the new version, validates it, flips the `active` symlink,
restarts, health-checks, and rolls back on failure. It is a *separate process
from the Rust core on purpose* — the component performing the switch cannot be
the binary being switched, so the supervisor must survive a core swap. It is kept
small and simple; it carries no agent logic.

The blue/green slots hold **core + its `.wasm` extensions** (the deploy unit).
UI client binaries are separate, optionally-installed artifacts updated on their
own — core runs headless without them.

## Directory layout

```
~/.jan-klod/
  jan-klod-supervisor  ← TinyGo updater (performs the flip; not swapped during a core update)
  blue/
    jan-klod         ← Rust core binary
    ext/
      provider-openai.wasm
      store-sqlite.wasm
      …
    jan-klod.yaml
  green/             ← standby slot (staged update)
    jan-klod         ← Rust core binary
    ext/
    jan-klod.yaml
  active             ← symlink → blue/ or green/
  state.yaml         ← records which slot is live and version history
```

## Update flow

1. Download new binary and/or `.wasm` extensions into the standby slot.
2. Validate (checksums, WASM component interface compatibility check).
3. Atomically flip the `active` symlink to the standby slot.
4. Restart the process.
5. Run health check.
6. **PASS** → keep new slot active, mark previous slot as rollback target.
7. **FAIL** → flip `active` back to previous slot, restart, alert user.

```
jan-klod update          — download + stage into standby
jan-klod update --apply  — flip symlink + restart
jan-klod rollback        — flip back to previous slot
```

## Extension-only updates

Individual `.wasm` files in `ext/` can be updated without replacing the core binary. The interface compatibility check ensures the new `.wasm` satisfies the same WIT world before the flip.

## State format (`state.yaml`)

```yaml
active: blue
blue:
  version: 1.2.0
  installed: 2026-06-16
  extensions:
    provider-openai: 0.3.1
    store-sqlite: 1.0.0
green:
  version: 1.3.0-rc1
  installed: 2026-06-28
  extensions:
    provider-openai: 0.4.0
    store-sqlite: 1.0.0
```
