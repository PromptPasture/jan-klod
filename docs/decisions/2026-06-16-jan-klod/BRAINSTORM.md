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

Jan-Klod is a minimal, stable AI agent core that does the splits between extensions. Inspired by the Linux kernel philosophy: the core is small, versioned, and almost never changes. Everything domain-specific — LLM providers, memory, UI, tools — lives in extensions that can be added, swapped, or disabled independently.

---

## Architecture

### Core

The core contains exactly four responsibilities and nothing else:

|Responsibility|Description|
|---|---|
|Config loader|Parses `jan-klod.yaml` into a typed config tree|
|Extension registry|Discovers extensions, resolves dependency graph, orders boot|
|Lifecycle manager|Drives `init → start → stop` and health checks|
|Event bus|Extension-to-extension communication|

Zero agent behavior in core. A core-only boot starts up and does nothing. That is intentional.

### Design principles

- **If it has variations, it is an extension.** LLM providers, context strategies, memory backends, UI — all extension territory.
- **If it will change frequently, it is an extension.** Agent loop logic, tool integrations, MCP protocol handling.
- **Core API surface must be frozen early.** Every interface core exposes to extensions is a contract. Breaking it breaks the ecosystem.
- **Extensions declare dependencies explicitly.** Core validates the graph at boot and refuses to start with unsatisfied hard dependencies.

---

## Extension Taxonomy

Naming convention: `{role}-{name}` where role encodes the type.

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

Shipped with core. These are the API surface that must not break.

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

Pre-built native binaries for the most common configurations. Bundle contents are fixed; active extensions are still controlled by `jan-klod.yaml`.

|Bundle|Contents|
|---|---|
|`bundle-tui`|all providers + manager-*+ store-memory + registry-* + tool-* + ui-terminal|
|`bundle-gui`|same, ui-graphical instead of ui-terminal|
|`bundle-full`|everything|
|*(jvm)*|core JAR only; user drops extension JARs into `ext/`|

Note: a combinatorial native build per extension selection is not feasible. Curated bundles are the native story; JVM mode covers custom combinations.

---

## Configuration

YAML only. No `.properties`, no JSON. Pattern borrowed from markdownlint: `extension-name: false` disables cleanly; a mapping configures it.

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

Skills follow the `.agents/skills/{skill-name}/SKILL.md` convention.

---

## Configurator

Web UI at `start.janklod.dev` (aspirational), modelled on Spring Initializr and Quarkus Dev. Generates and downloads a ZIP.

### UI flow

```
1. Version        [1.0.0 ▾]

2. Packaging      ( ) jvm   — flexible, any extension combo, user builds
                  (•) tui   — native binary, terminal UI
                  ( ) gui   — native binary, graphical UI
                  ( ) full  — native binary, everything

3. LLM Provider   [x] Anthropic  [ ] OpenAI  [ ] Ollama
   (jvm only; greyed out for bundles)

4. Extensions     [x] Context    [x] Memory   [x] Skills
                  [x] MCP        [ ] Web Search
   (jvm only; greyed out for bundles)

5. UI             (•) Terminal   ( ) Graphical
   (jvm only)

                  [ Generate & Download ]
```

### ZIP layout — JVM

```
jan-klod.zip
├── jan-klod.yaml          ← pre-filled from selections
├── lib/
│   ├── core-{version}.jar
│   ├── provider-anthropic-{version}.jar
│   └── ...selected extensions...
├── ext/                   ← drop additional JARs here
├── jan-klod               ← launcher (Unix)
├── jan-klod.bat           ← launcher (Windows)
└── README.md
```

### ZIP layout — Bundle

```
jan-klod-tui-{version}-linux-amd64.zip
├── jan-klod               ← native binary
├── jan-klod.yaml          ← pre-filled config
└── README.md
```

---

## Blue/Green Deployment

A tiny launcher binary (Go or Rust, no dependencies) manages version lifecycle. The launcher itself almost never needs updating.

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

SQLite is the default for `store-memory` — portable, embeddable, migration-friendly. State schema must be versioned from day one; retrofitting is painful.

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
