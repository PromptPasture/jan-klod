---
type: decision
title: Jan-Klod Brainstorm — Original Design (Java/Quarkus era)
description: Distilled architecture, artifact map, contracts sketch, config schema, bundle strategy, and blue/green design from the 2026-06-15 brainstorm. Superseded; kept for history.
tags: [decision, brainstorm, architecture, java, quarkus, superseded]
created: 2026-06-16
updated: 2026-06-16
---

# Brainstorm

## Vision

Jan-Klod: minimal, stable core that splits to extensions (Linux kernel model). Core is small, versioned, rarely changes. Domain-specific parts (LLM providers, memory, UI, tools) live in independent, swappable extensions.

---

## Architecture

### Core

Four responsibilities, nothing else:

|Responsibility|Description|
|---|---|
|Config loader|Parses `jan-klod.yaml` into typed config|
|Extension registry|Discovers, wires, orders by dependency|
|Lifecycle manager|`init → start → stop`, health checks|
|Event bus|Extension communication|

Core has zero agent behavior. Core-only boot: starts, does nothing (intentional).

### Design principles

- **Variations → extensions.** LLM providers, strategies, backends, UI.
- **Frequent changes → extensions.** Loop logic, tools, MCP.
- **Freeze core API early.** Every exposed interface is a contract; breaking it breaks ecosystem.
- **Explicit dependencies.** Core validates graph at boot; rejects unsatisfied hard deps.

---

## Extension Taxonomy

Naming: `{role}-{name}` encodes type.

|Role prefix|Meaning|
|---|---|
|`provider-*`|LLM API clients|
|`manager-*`|Stateful orchestrators|
|`store-*`|Persistence backends|
|`registry-*`|Catalogues of external capabilities|
|`tool-*`|Discrete callable tools|
|`user-interface-*`|Human-facing surfaces|
|`bundle-*`|Pre-packaged native distributions|

---

## Artifact Map

Group ID: `com.github.janklod`

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

---

## Contracts (Stable Interfaces)

Ship with core; must not break.

```java
interface Extension {
    String id();
    default List<String> requires() { return List.of(); }
    void init(ExtensionContext ctx);
    void start();
    void stop();
    default HealthStatus health() { return HealthStatus.UP; }
}

interface ExtensionContext {
    Config config();
    EventBus events();
    <T> T require(Class<T> contract);       // hard dependency — fails at boot if absent
    <T> Optional<T> optional(Class<T>);    // soft dependency
}

// Contracts implemented by extensions
interface LlmProvider      { ... }   // ← provider-*
interface ContextManager   { ... }   // ← manager-context
interface AgentManager     { ... }   // ← manager-agent-loop
interface MemoryStore      { ... }   // ← store-memory
interface SkillRegistry    { ... }   // ← registry-skills
interface McpRegistry      { ... }   // ← registry-mcp
```

---

## Extension Dependency Graph

```
provider-anthropic  ─┐
provider-openai     ─┤ implements LlmProvider
provider-ollama     ─┘

manager-context       implements ContextManager

manager-agent-loop    requires:  LlmProvider, ContextManager
                      optional:  MemoryStore, SkillRegistry, McpRegistry
                      implements AgentManager

store-memory          implements MemoryStore

registry-skills       implements SkillRegistry
registry-mcp          implements McpRegistry

tool-web-search       requires:  AgentManager (registers itself as a tool)

user-interface-terminal   requires: AgentManager
user-interface-graphical  requires: AgentManager
```

---

## Bundles

Pre-built native binaries; contents fixed but controlled by `jan-klod.yaml`.

|Bundle|Contents|
|---|---|
|`bundle-tui`|all providers + manager-*+ store-memory + registry-* + tool-* + ui-terminal|
|`bundle-gui`|same, ui-graphical instead of ui-terminal|
|`bundle-full`|everything|
|*(jvm)*|core JAR only; user drops extension JARs into `ext/`|

Note: combinatorial native builds infeasible. Curated bundles for native; JVM mode for custom combos.

---

## Configuration

YAML only (markdownlint pattern): `extension-name: false` disables; mapping configures.

```yaml
jan-klod:
  version: 1.0.0
  data-dir: ~/.jan-klod

extensions:

  provider-anthropic:
    model: claude-sonnet-4-6
    api-key: ${ANTHROPIC_API_KEY}
    max-tokens: 8192

  provider-openai: false
  provider-ollama: false

  manager-context:
    strategy: sliding-window
    max-tokens: 100000
    preserve-system: true

  manager-agent-loop: true

  store-memory:
    backend: sqlite
    path: ${jan-klod.data-dir}/memory.db

  registry-skills:
    path: .agents/skills/
    auto-discover: true

  registry-mcp:
    servers:
      - name: filesystem
        transport: stdio
        command: npx -y @modelcontextprotocol/server-filesystem /home
      - name: github
        transport: sse
        url: https://mcp.github.com/sse

  tool-web-search:
    provider: brave
    api-key: ${BRAVE_API_KEY}

  user-interface-terminal:
    theme: dark
    vim-keys: false

  user-interface-graphical: false
```

Skills: `.agents/skills/{skill-name}/SKILL.md`.

---

## Configurator

Web UI (start.janklod.dev, aspirational): Spring Initializr / Quarkus Dev model. Generate & download ZIP.

**Flow:** Version → Packaging (jvm/tui/gui/full) → LLM Provider → Extensions → UI → Download

### ZIP layout — JVM

```
jan-klod.zip
├── jan-klod.yaml
├── lib/
│   ├── core-{version}.jar
│   ├── provider-anthropic-{version}.jar
│   └── ...extensions...
├── ext/
├── jan-klod (Unix) / jan-klod.bat (Windows)
└── README.md
```

### ZIP layout — Bundle

```
jan-klod-tui-{version}-linux-amd64.zip
├── jan-klod
├── jan-klod.yaml
└── README.md
```

---

## Blue/Green Deployment

Tiny launcher (Go/Rust, no deps) manages version lifecycle; rarely needs updates.

### Directory layout

```
~/.jan-klod/
├── launcher               ← tiny supervisor, rarely changes
├── current  -> versions/1.2.0/
├── previous -> versions/1.1.0/
└── versions/
    ├── 1.1.0/
    │   ├── jan-klod       ← binary or lib/
    │   └── state/         ← memory snapshot, config at that version
    └── 1.2.0/
        ├── jan-klod
        └── state/
```

### Update flow

```
1. Download new version → versions/{new}/
2. Migrate state from previous version (schema migration)
3. Start new version, run health check
4. PASS → flip current symlink, keep previous as rollback
5. FAIL → stay on current, mark new version bad, alert user
6. User can always run: jan-klod rollback
```

### State format

SQLite default: portable, embeddable, migration-friendly. Version schema from day one (retrofitting painful).

---

## Open Questions

- [ ] Does `manager-agent-loop` support streaming responses, or is that a separate contract?
- [ ] How do multiple LLM providers coexist — is only one active, or can the loop select per-request?
- [ ] Extension versioning: does each extension version independently, or does a Jan-Klod release version all together?
- [ ] How does `registry-mcp` handle MCP server restarts / crashes?
- [ ] Launcher implementation language: Go vs Rust vs shell?
- [ ] Config hot-reload: can extensions pick up YAML changes without restart?
- [ ] Extension isolation: classloader-per-extension, or flat classpath (JVM mode)?
- [ ] Configurator hosting: self-hosted only, or a public `start.janklod.dev`?

---

## Decided

|Decision|Choice|Rationale|
|---|---|---|
|Agent loop|Extension (`manager-agent-loop`)|Will change; keeps core frozen|
|LLM clients|Extensions (`provider-*`)|Multiple providers, different cadences|
|Context management|Extension (`manager-context`)|Strategy may vary|
|Config format|YAML only|Human-readable, markdownlint-style `false` toggle|
|Native strategy|Curated bundles, not per-combo|2^n combinations not feasible|
|State backend|SQLite|Portable, embeddable, migration-friendly|
|Naming convention|`{role}-{name}`|Role encoded in artifact name|
|Group ID|`com.github.janklod`|GitHub-aligned|
|Skills location|`.agents/skills/`|Consistent with `.agents/` convention|
