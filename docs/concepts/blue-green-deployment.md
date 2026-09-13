---
type: concept
title: Blue/Green Deployment
description: Zero-downtime update strategy for Jan-Klod runtime and extensions
tags: [deployment, updates, rollback]
created: 2026-06-28T00:00:00Z
updated: 2026-06-29T00:00:00Z
---

Jan-Klod uses blue/green deployment for zero-downtime updates and instant rollback.

A **small standalone supervisor** (TinyGo binary) performs updates: stages, validates, flips `active`, restarts, health-checks, and rolls back on failure. Separate from the core — the switcher cannot be the binary being switched. Kept small, simple, and free of agent logic.

Blue/green slots hold **core + `.wasm` extensions**. UI client binaries are separate and updated independently.

## Directory layout

```
~/.jan-klod/
  jan-klod-supervisor  ← updater
  blue/
    jan-klod         ← core binary
    ext/
      …extensions…
    config.yaml
  green/             ← standby slot
    jan-klod
    ext/
    config.yaml
  active             ← symlink → blue/ or green/
  state.yaml         ← version history
```

## Update flow

1. Download new binary and/or `.wasm` extensions to standby.
2. Validate (checksums, WASM compatibility).
3. Atomically flip `active` to standby.
4. Restart, then health-check.
5. **PASS** → keep new slot
6. **FAIL** → flip back, restart

```
jan-klod update         — download + stage
jan-klod update --apply — flip + restart
jan-klod rollback       — flip back
```

## Extension-only updates

Individual `.wasm` files can update without replacing the core. Interface compatibility checks ensure the new `.wasm` satisfies the same WIT world before the flip, letting extensions update independently of the core binary.

## State format (`state.yaml`)

```yaml
active: blue
blue:
  version: 1.2.0
  installed: 2026-06-16
  extensions:
    provider-openai: 0.3.1
    interceptor-intent-router: 1.0.0
green:
  version: 1.3.0-rc1
  installed: 2026-06-28
  extensions:
    provider-openai: 0.4.0
    interceptor-intent-router: 1.0.0
```
