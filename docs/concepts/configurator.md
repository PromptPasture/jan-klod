---
type: concept
title: Configurator
description: Web UI for generating Jan-Klod configuration and deployment archives
tags: [configurator, ui, zip, setup]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T00:00:00Z
---

The Configurator is a Spring Initializr / Quarkus-style web UI. Users select extensions and provide settings; the UI generates a ready-to-run archive.

## UI flow

1. Pick deployment mode: JVM or native bundle.
2. Select extensions from the registry (search, filter by category).
3. Fill in configuration values (LLM endpoint, API keys, etc.).
4. Click **Generate** — the server resolves the dependency graph and builds the ZIP.

## ZIP layout — JVM mode

```
jan-klod-<version>/
  jan-klod.yaml       ← generated config
  lib/
    jan-klod-core-*.jar
    jan-klod-<ext>-*.jar
    …
  ext/                ← drop folder for additional extensions
  bin/
    jan-klod           (launcher shell script)
    jan-klod.bat
```

## ZIP layout — bundle mode

```
jan-klod-<version>/
  jan-klod             ← GraalVM native binary (all selected extensions linked in)
  jan-klod.yaml
```
