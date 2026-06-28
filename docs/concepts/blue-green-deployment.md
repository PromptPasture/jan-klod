---
type: concept
title: Blue/Green Deployment
description: Zero-downtime update strategy for Jan-Klod runtime and extensions
tags: [deployment, updates, rollback]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T00:00:00Z
---

Jan-Klod uses a blue/green strategy to update the runtime or extensions without downtime and with instant rollback.

The **core Go binary** acts as its own launcher — no separate supervisor binary is needed.

## Directory layout

```
~/.jan-klod/
  blue/
    jan-klod         ← Go binary
    ext/
      provider-openai.wasm
      store-sqlite.wasm
      …
    jan-klod.yaml
  green/             ← standby slot (staged update)
    jan-klod
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
