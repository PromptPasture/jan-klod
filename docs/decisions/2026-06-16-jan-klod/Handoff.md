---
type: decision
title: Jan-Klod Initial Design (Java/Quarkus era)
description: Project vision, artifact map, extension taxonomy, config schema, and blue/green design — the original Java/Quarkus plan. Superseded by 2026-06-28 onward.
tags: [decision, architecture, jan-klod, java, quarkus, superseded]
created: 2026-06-16
updated: 2026-06-16
---

# Handoff — 🥷 Jan Klod Agent Project

## What this is

**Jan-Klod** brainstorm: minimal, stable Java/Quarkus core + typed extensions + curated bundles + Initializr-style configurator.

Artifacts:
- [`BRAINSTORM.md`](BRAINSTORM.md) — distilled architecture, contracts, config, bundles, deployment, questions
- [`CONVERSATION.md`](CONVERSATION.md) — verbatim transcript (primary source)

Reference these; do not re-derive.

---

## Project snapshot

- **Name:** Jan-Klod
- **Emoji:** 🥷
- **Group ID:** `com.github.janklod`
- **Stack:** Java, Quarkus, GraalVM native, YAML config
- **Philosophy:** Linux kernel — core is a container only, all domain logic in extensions

### Core (only)

- YAML config loader
- Extension registry (dependency graph, boot order)
- Lifecycle manager (`init → start → stop → health`)
- Event bus

Zero agent behavior. Core-only boot: no-op.

### Extension naming convention

`{role}-{name}` — role prefix encodes type:

|Prefix|Role|
|---|---|
|`provider-*`|LLM API clients|
|`manager-*`|Stateful orchestrators|
|`store-*`|Persistence backends|
|`registry-*`|Capability catalogues|
|`tool-*`|Callable tools|
|`user-interface-*`|Human-facing surfaces|

### Full artifact list

```
com.github.janklod:core

com.github.janklod:provider-anthropic
com.github.janklod:provider-openai
com.github.janklod:provider-ollama

com.github.janklod:manager-context
com.github.janklod:manager-agent-loop

com.github.janklod:store-memory

com.github.janklod:registry-skills
com.github.janklod:registry-mcp

com.github.janklod:tool-web-search

com.github.janklod:user-interface-terminal
com.github.janklod:user-interface-graphical

com.github.janklod:bundle-tui
com.github.janklod:bundle-gui
com.github.janklod:bundle-full
```

### Key dependency edges

```
manager-agent-loop  requires:  LlmProvider, ContextManager
                    optional:  MemoryStore, SkillRegistry, McpRegistry
user-interface-*    requires:  AgentManager
tool-web-search     requires:  AgentManager
```

### Bundling

- `jvm` — JAR only, drop extensions into `ext/`
- `bundle-tui` — native: all components + terminal UI
- `bundle-gui` — all + graphical UI
- `bundle-full` — everything

Combinatorial native builds infeasible; curated bundles only.

### Config format

YAML only. markdownlint-style toggle: `extension-name: false` disables, a mapping configures. See `BRAINSTORM.md` for full example.

Skills path convention: `.agents/skills/{skill-name}/SKILL.md`

### Blue/green deployment

Tiny launcher (Go/Rust):
- Symlink: `current → versions/{n}/`, `previous → versions/{n-1}/`
- Health check + auto-rollback on failure
- Manual: `jan-klod rollback`

State: SQLite (portable, migration-friendly). Version schema from day one.

### Configurator

Initializr-style web UI. Generates ZIP:
- JVM: config + libs + launchers + ext folder
- Bundle: native binary + config

---

## Open questions (priority order)

1. **Multi-provider** — one active or per-request selection? (affects `LlmProvider` contract)
2. **Extension versioning** — independent or BOM release?
3. **Streaming** — loop responsibility or separate contract?
4. **MCP fault tolerance** — crash/restart handling?
5. **Config hot-reload** — without restart?
6. **Classloader isolation** — per-extension or flat?
7. **Launcher language** — Go/Rust/shell?
8. **Configurator** — self-hosted or public?

---

## Next: Contracts API Design

Define interfaces (`LlmProvider`, `ContextManager`, `AgentManager`, `MemoryStore`, `SkillRegistry`, `McpRegistry`) for Maven scaffolding. Resolve open questions 1–3 first (shape signatures).

---

## Suggested skills

- **`brainstorm`** — if more design needed
- **`plan`** — sequence build phases (contracts settled)
- **`write-ticket`** — break into work items
- **`review`** — evaluate contracts draft

---

## Notes

- Artifact names & group ID are deliberate; don't change without explicit user instruction.
- YAML only (no .properties/JSON).
- Read `.agents/memory/MEMORY.md` at session start.
- No secrets shared.
