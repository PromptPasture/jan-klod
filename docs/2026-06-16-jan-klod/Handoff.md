---
generated: 2026-06-16
---

# Handoff — 🥷 Jan Klod Agent Project

## What this is

A brainstorm session for **Jan-Klod** — a minimal, stable AI agent core built on Java/Quarkus, extensible via a typed plugin system, with curated native bundles and a Spring-Initializr-style configurator.

The primary output artifact from this session is:

- `BRAINSTORM.md` — full architecture decisions, artifact map, config schema, bundle strategy, blue/green deployment design, and open questions. _(presented to user as a download — ask them for the file path if needed)_

Do not re-derive decisions already captured there. Reference it as the source of truth.

---

## Project snapshot

- **Name:** Jan-Klod
- **Emoji:** 🥷
- **Group ID:** `com.github.janklod`
- **Stack:** Java, Quarkus, GraalVM native, YAML config
- **Philosophy:** Linux kernel — core is a container only, all domain logic in extensions

### Core contains (only)

- YAML config loader
- Extension registry (dependency graph, ordered boot)
- Lifecycle manager (`init → start → stop → health`)
- Event bus

Zero agent behavior in core. Core-only boot does nothing.

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

- `jvm` — core JAR only, user drops extension JARs into `ext/`
- `bundle-tui` — native: all providers + managers + store + registries + tools + terminal UI
- `bundle-gui` — same, graphical UI
- `bundle-full` — everything

Combinatorial native builds are not feasible. Curated bundles only for native.

### Config format

YAML only. markdownlint-style toggle: `extension-name: false` disables, a mapping configures. See `BRAINSTORM.md` for full example.

Skills path convention: `.agents/skills/{skill-name}/SKILL.md`

### Blue/green deployment

Tiny launcher binary (Go or Rust) manages:

- `~/.jan-klod/current → versions/{n}/` symlink
- `~/.jan-klod/previous → versions/{n-1}/` rollback
- Health check after update; auto-rollback on failure
- `jan-klod rollback` for manual rollback

State backend: SQLite (portable, migration-friendly). Schema must be versioned from day one.

### Configurator

Spring-Initializr / Quarkus-style web UI. Generates a ZIP:

- JVM: `jan-klod.yaml` + `lib/*.jar` + launcher scripts + `ext/` drop folder
- Bundle: `jan-klod` native binary + `jan-klod.yaml`

---

## Open questions (unresolved — tackle next)

These are in priority order for design impact:

1. **Multi-provider coexistence** — is only one `LlmProvider` active at a time, or can `manager-agent-loop` select per-request? Affects the `LlmProvider` contract.
2. **Extension versioning** — does each extension version independently, or does a Jan-Klod release version all artifacts together (BOM)?
3. **Streaming** — does `manager-agent-loop` handle streaming, or is that a separate contract / capability flag on `LlmProvider`?
4. **MCP fault tolerance** — how does `registry-mcp` handle server crashes/restarts?
5. **Config hot-reload** — can extensions react to YAML changes without restart?
6. **JVM classloader isolation** — one classloader per extension or flat classpath?
7. **Launcher language** — Go vs Rust vs shell?
8. **Configurator hosting** — self-hosted only, or a public `start.janklod.dev`?

---

## Suggested next steps

The natural next session is **contracts API design** — define the Java interfaces (`LlmProvider`, `ContextManager`, `AgentManager`, `MemoryStore`, `SkillRegistry`, `McpRegistry`) precisely enough to start scaffolding the Maven multi-module project.

Resolve open questions 1–3 first, as they directly shape the interface signatures.

---

## Suggested skills

- **`brainstorm`** — if more design space needs exploring before coding
- **`plan`** — once contracts are settled, to sequence the build phases
- **`write-ticket`** — to break the build into tracked work items
- **`review`** — to evaluate any contracts draft before committing to it

---

## Notes for next agent

- Do not rename artifacts or change the group ID without explicit user instruction. Both were deliberate decisions made in this session.
- The user prefers YAML over `.properties` or JSON — enforce this in any config scaffolding.
- `AGENTS.md` in the repo root defines memory and behavior conventions; read `.agents/memory/MEMORY.md` and today's daily note at session start.
- No sensitive information was shared in this session.
