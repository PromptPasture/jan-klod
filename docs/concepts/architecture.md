---
type: concept
title: Architecture
description: High-level architecture of the Jan-Klod agent runtime
tags: [architecture, core, extensions, quarkus]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T00:00:00Z
---

## Philosophy

Linux kernel model: the **core** is a minimal container with no domain logic. All agent behaviour is provided by **extensions** loaded at runtime.

## Core responsibilities

- Extension lifecycle management (load, enable, disable, unload)
- Configuration loading (`jan-klod.yaml`)
- Dependency injection container (Quarkus CDI)
- Contract interfaces exposed to extensions (see [Contracts](contracts.md))

## Extensions

Extensions are JAR files (JVM) or shared libraries (native) dropped into `ext/`. They declare dependencies on other extensions and on core contracts.

### Extension naming convention

```
jan-klod-<category>-<name>
```

Categories: `llm`, `context`, `memory`, `skill`, `mcp`, `tool`, `ui`

### Extension dependency graph

```
manager-agent-loop  requires:  LlmProvider, ContextManager
                    optional:  MemoryStore, SkillRegistry, McpRegistry
user-interface-*    requires:  AgentManager
tool-web-search     requires:  AgentManager
```

## Agent loop architecture

```
User query
    │
    ▼
Intent router ──→ direct answer (no agent)
    │
    ▼
Step controller (selects tools, compresses history, builds prompt)
    │
    ▼
LLM (constrained decoding)
    │
    ▼
Parse & validate action → retry on failure
    │
    ▼
Tool execution → loop back to step controller
    │
    ▼
Answer extractor
```

## Stack

| Layer | Technology |
|---|---|
| Runtime | Java 21, Quarkus |
| Native binary | GraalVM native-image |
| Config | YAML (`jan-klod.yaml`) |
| Group ID | `com.github.janklod` |

## Deployment modes

- **JVM:** `jan-klod.yaml` + `lib/*.jar` + launcher scripts + `ext/` drop folder
- **Bundle (native):** single `jan-klod` binary + `jan-klod.yaml`

See [Blue/Green Deployment](blue-green-deployment.md) for the update strategy.
