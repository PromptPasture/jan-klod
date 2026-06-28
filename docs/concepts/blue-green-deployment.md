---
type: concept
title: Blue/Green Deployment
description: Zero-downtime update strategy for Jan-Klod runtime and extensions
tags: [deployment, updates, rollback]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T00:00:00Z
---

Jan-Klod uses a blue/green strategy to update the runtime or extensions without downtime and with instant rollback.

## Directory layout

```
~/.jan-klod/
  blue/          ← currently active slot
    jan-klod     (binary or JARs)
    ext/
  green/         ← standby / new version
    jan-klod
    ext/
  active         ← symlink → blue/ or green/
  state.yaml     ← records which slot is live
```

## Update flow

1. Download new version into the standby slot.
2. Validate (checksum, smoke test).
3. Atomically flip the `active` symlink to the standby slot.
4. Restart the process (or hot-reload if supported).
5. On failure, flip `active` back to the previous slot and restart.

## State format (`state.yaml`)

```yaml
active: blue
blue:
  version: 1.2.0
  installed: 2026-06-16
green:
  version: 1.3.0-rc1
  installed: 2026-06-28
```
