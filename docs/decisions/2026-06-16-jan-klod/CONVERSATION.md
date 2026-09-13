---
type: source
title: Jan-Klod — Original Brainstorm Conversation (verbatim)
description: Full transcript of the 2026-06-15 brainstorm that produced the 2026-06-16 Jan-Klod design. Java/Quarkus era — superseded by later decisions, kept as the primary source.
tags: [source, transcript, brainstorm, jan-klod, java, quarkus, superseded]
created: 2026-06-15
updated: 2026-06-29
---

# Jan-Klod — Full Brainstorm Conversation

> Created: 2026-06-15 · **Primary source** for [Handoff.md](Handoff.md) and [BRAINSTORM.md](BRAINSTORM.md).
> Raw transcript (Java/Quarkus era); see [2026-06-29-component-model-rust](../2026-06-29-component-model-rust/Handoff.md)
> for current Rust/Wasmtime/Component Model architecture. Load-bearing ideas: Linux-kernel core, 
> agent-loop as extension, LSP-style server/client split, minimal launcher.

---

## Is it possible to do Agent harness on Java? For example, Quarkus framework. What advantages and disadvantages will it have against common Claude Code, OpenCode and others?

**Assistant:**

### Java/Quarkus as an Agent Harness

Yes, absolutely viable — and there's already a solid foundation in **LangChain4j's Quarkus extension**. Here's an honest breakdown.

#### Quarkus agent harness

- `quarkus-langchain4j` — declarative services, tools, memory, embeddings
- Mutiny + Vert.x — streaming LLM responses
- MicroProfile REST Client — Anthropic API integration
- CDI — dependency injection
- GraalVM native — fast binary

Minimal loop:

```java
@ApplicationScoped
@RegisterAiService(tools = { FileSystemTool.class, BashTool.class })
public interface AgentService {
    @SystemMessage("You are a coding agent. Use tools to complete tasks.")
    @UserMessage("{input}")
    Uni<String> run(String input);
}
```

Skills load as prompt segments via `@SystemMessage` or custom `ChatMemory`.

#### Advantages vs. Claude Code / OpenCode / Python agents

| Dimension | Quarkus | Claude Code (TS) | OpenCode (Go) | Python (LangChain) |
|---|---|---|---|---|
| **Startup** | ~5ms native, ~1s JVM | ~300ms | ~10ms | ~800ms+ |
| **Type safety** | ✅ Strong | ✅ Strong | ✅ Strong | ❌ Weak |
| **Enterprise integration** | ✅ Best-in-class | ❌ | ❌ | Moderate |
| **Streaming** | ✅ Reactive native | ✅ | ✅ | Varies |
| **Observability** | ✅ OTEL built-in | Manual | Manual | Manual |
| **MCP ecosystem** | ⚠️ Partial | ✅ First-class | ⚠️ Partial | ✅ Good |
| **AI tooling ecosystem** | Thin | Moderate | Thin | ✅ Richest |
| **Memory/vector stores** | ✅ via LangChain4j | Limited | Limited | ✅ Richest |
| **Skills loading (dynamic)** | ⚠️ GraalVM friction | ✅ Easy | ✅ Easy | ✅ Easy |

**Wins:** Enterprise auth/secrets/DB/audit/multi-tenancy, REST/gRPC orchestration, built-in OTEL tracing, Kubernetes/small images.

**Loses:** MCP lags TS/Python, GraalVM reflection friction (runtime-discovered SKILL.md needs JVM mode), sparse ecosystem, slow iteration, no editor integration vs. Claude Code/OpenCode.

#### GraalVM friction

Runtime-discovered Markdown vs. fixed classpath at build time. **Options:**

1. JVM mode — no friction, dynamic loading, ~1s startup (fine for servers)
2. Build-time bundling — fast native, no runtime installs
3. Hybrid — watched directory reload (adds native config complexity)

**Good fit:** Server-side platform, Java shop, multi-tenancy, high concurrency.
**Poor fit:** Local CLI agent, rapid iteration, Python ML libraries, cutting-edge MCP.

**Recommendation:** Server platform → competitive. Local agent → TypeScript/Go better (CLI ecosystem, startup, MCP tooling).

---

## Usually skills are stored in .agents/skills folder

**Assistant:**

Aligns with the `.agents/` convention in `AGENTS.md`. Full layout:

```
.agents/
├── memory/
│   ├── MEMORY.md
│   └── 2026-06-15.md
└── skills/
    ├── frontend-design/
    │   └── SKILL.md
    ├── review-code/
    │   └── SKILL.md
    └── your-skill/
        ├── SKILL.md
        ├── scripts/
        └── references/
```

Agent startup scans `.agents/skills/` for `SKILL.md` files. In JVM mode, trivial with `Files.walk()`. In GraalVM native, pre-index at build time or configure native image filesystem access.

Yes, use `.agents/skills/` going forward.

---

## Wait. We are brainstorming yet. How hard will it be to support extensions? I'm thinking on making something small as Pi Agent, just a core, like Linux, and expand it via extensions

**Assistant:**

Good framing. The Linux kernel analogy maps well — core handles the agent loop, context, and API communication; everything else is a module.

### Extension layers

Separate by type before evaluating complexity:

```
┌─────────────────────────────────┐
│  Skills (.agents/skills/)       │  ← Markdown, zero code
├─────────────────────────────────┤
│  Tools (file, bash, browser...) │  ← Executable capabilities
├─────────────────────────────────┤
│  MCP servers (external procs)   │  ← Protocol-based, any language
├─────────────────────────────────┤
│  Core (agent loop, context, LLM)│  ← Your "kernel"
└─────────────────────────────────┘
```

Each layer has a different extension story.

### How hard each layer is

**Skills — trivial.** Filesystem scanning; no API surface.

**MCP servers — nearly free.** Protocol is the boundary; any language/process. Just track which servers are active.

**Tools — moderate.** Java's `ServiceLoader` works; hard part is a stable tool API that won't break with core evolution.

**Core hooks — hard.** Intercepting the loop (pre/post tool calls, memory backends, routing) requires proper lifecycle with defined hook points. Scope creep risk.

### The real risk

Linux took decades to stabilize module APIs. **Premature API surface** is the threat — every hook point becomes a contract. Safer: start with zero hooks, only skills + MCP, add hooks based on real usage pressure.

### Java-specific friction

GraalVM native and JAR runtime loading conflict. Drop-in JAR extensions require JVM mode (acceptable for persistent agent; sub-10ms startup not needed).

### Next: extension boundary

Tools (code) vs. skills+MCP (config/protocol)? That choice drives architecture.

---

## I want to make a core + extensions: skills support, mcp support, memory, web,.. And make a configurator like Spring Boot or Quarkus has - user choose version, packaging (jar/native), set of packages (jar will come with, native will be from pre-built set). Also want to make it blue/green deployment on update, if something goes wrong, user will always have working instance

**Assistant:**

Ambitious architecture. Mapping shape and hard problems:

### Overall shape

```
┌─────────────────────────────────────────────┐
│           Configurator (web UI)             │
│  version · packaging · extensions · download│
└──────────────────┬──────────────────────────┘
                   │ generates / downloads
       ┌───────────┴───────────┐
       │  JAR build            │  Native binary
       │  (any combo,          │  (pre-built bundles,
       │   user builds)        │   instant download)
       └───────────┬───────────┘
                   │
┌──────────────────▼──────────────────────────┐
│                  Core                       │
│  agent loop · LLM client · context mgmt     │
├──────────────┬──────────────┬───────────────┤
│ skills-ext   │  mcp-ext     │  memory-ext   │
├──────────────┴──────────────┴───────────────┤
│              web-ext · tools-ext · ...      │
└─────────────────────────────────────────────┘
```

### Native combinatorial problem

10 extensions = 1,024 combos. Can't pre-build all. **Three options:**

**A) Curated bundles** — like Linux distro editions

```
minimal   = core + skills + mcp
standard  = minimal + memory + web
full      = standard + tools + browser
```

**B) Core native + extensions as JARs** — hybrid model

```
pi-agent (native, fast startup)
  └── loads extensions/*.jar via embedded JVM on demand
```

**C) JAR-only, forget native for extensible builds**

Option A is probably right for v1.

### Blue/green for a local agent

```
~/.pi-agent/
├── launcher          ← tiny binary, never changes
├── current -> versions/1.2.0/
├── previous -> versions/1.1.0/
└── versions/
    ├── 1.1.0/
    │   ├── pi-agent
    │   └── state/
    └── 1.2.0/
        ├── pi-agent
        └── state/
```

Launcher flow on update:
```
1. Download 1.2.0 → versions/1.2.0/
2. Migrate state 1.1.0 → 1.2.0 (schema migration)
3. Start 1.2.0, run health check
4. If OK → flip current symlink, keep previous
5. If fail → stay on current, mark 1.2.0 bad
6. User can rollback: launcher rollback
```

The launcher itself should be a tiny Go or Rust binary — something that almost never needs updating and has zero dependencies.

### Real complexity

**State migration.** Every upgrade needs paths for schemas, configs, history. Design for portability from day one.

**Extension API stability.** Every exposed interface is a contract. Freeze minimal surface early; version explicitly.

**Configurator.** Form that generates `pom.xml` or downloads binary. Fork Spring Initializr.

### Decide next: CLI vs. server? Native vs. hybrid? Core scope? SQLite for state?

---

## Shape / Bundling decisions

**User:**

```
## Shape

I want to make the core really small and stable and push as much as we can to the extensions.

* llm client - whether it will be openai or claude or ollama or other => extension
* context mgmt - if it will change => extension

Any thing that will have variations or will change frequently is a good signal to move to extension.

## Bundling

* jvm = jars
* tui = core + skills + mcp + memory + web search + terminal ui
* gui = same, but graphical ui
* full

I also want to have configuration in YAML, no .properties or JSON.
I like markdownlint example, where you can turn off rules with simple `false` and configure each rule if needed.
```

**Assistant:**

### What core actually is now

If LLM client and context management are extensions, core shrinks to just four responsibilities:

```
Core
├── Config loader        (YAML → typed config tree)
├── Extension registry   (discover, wire, order by dependency)
├── Lifecycle manager    (init → start → stop, health checks)
└── Event bus            (extension-to-extension communication)
```

That's it. No domain logic. Genuinely stable.

### Extension dependency declaration

```java
public interface Extension {
    String id();
    default List<String> requires() { return List.of(); }
    void init(ExtensionContext ctx);
    void start();
    void stop();
    default HealthStatus health() { return HealthStatus.UP; }
}

public interface ExtensionContext {
    Config config();
    EventBus events();
    <T> T require(Class<T> contract);
    <T> Optional<T> optional(Class<T>);
}
```

### YAML config — markdownlint style

```yaml
pi-agent:
  version: 1.0.0
  data-dir: ~/.pi-agent

extensions:

  llm:
    provider: claude
    model: claude-sonnet-4-6
    api-key: ${ANTHROPIC_API_KEY}

  context: false

  # Or configure:
  context:
    strategy: sliding-window
    max-tokens: 100000
    preserve-system: true

  memory:
    backend: sqlite
    path: ${data-dir}/memory.db

  skills:
    path: .agents/skills/
    auto-discover: true

  mcp:
    servers:
      - name: filesystem
        transport: stdio
        command: npx -y @modelcontextprotocol/server-filesystem /home

  web-search:
    provider: brave
    api-key: ${BRAVE_API_KEY}

  tui:
    theme: dark
    vim-keys: true

  web:
    port: 8080
    cors: false
```

### The one thing to decide now

**A) Agent loop is also an extension** — core is truly just a container.
**B) Agent loop is in core** — simplest viable thing.

Option A is the principled choice for the Linux analogy. Which way are you leaning?

---

## Name will be "Jan-Klod". Agent loop - option A

**Assistant:**

Love it. Very on-brand for something that does the splits between extensions.

### What Jan-Klod core now contains

Exactly four things, nothing else:

```
jan-klod-core
├── YAML config loader
├── Extension registry   (dependency graph, load order)
├── Lifecycle manager    (init → start → stop → health)
└── Event bus
```

Zero agent behavior in core. If you ship core alone, it boots and does nothing.

### Extension graph

```
contracts (stable interfaces — ships with core)
│
├── LlmProvider
├── ContextManager
├── MemoryStore
├── SkillRegistry
├── McpRegistry
└── AgentLoop

extensions
├── llm-claude      implements LlmProvider
├── llm-openai      implements LlmProvider
├── llm-ollama      implements LlmProvider
├── context-window  implements ContextManager
├── memory-sqlite   implements MemoryStore
├── skills          implements SkillRegistry
├── mcp             implements McpRegistry
├── web-search
│
├── agent-loop      requires: LlmProvider, ContextManager
│                   optional: MemoryStore, SkillRegistry, McpRegistry
│
├── tui             requires: AgentLoop
└── gui             requires: AgentLoop
```

### Artifact IDs sketch

```
dev.janklod:jan-klod-core
dev.janklod:jan-klod-ext-llm-claude
dev.janklod:jan-klod-ext-llm-openai
dev.janklod:jan-klod-ext-llm-ollama
dev.janklod:jan-klod-ext-context-window
dev.janklod:jan-klod-ext-memory-sqlite
dev.janklod:jan-klod-ext-skills
dev.janklod:jan-klod-ext-mcp
dev.janklod:jan-klod-ext-web-search
dev.janklod:jan-klod-ext-agent-loop
dev.janklod:jan-klod-ext-tui
dev.janklod:jan-klod-ext-gui
dev.janklod:jan-klod-bundle-tui
dev.janklod:jan-klod-bundle-gui
dev.janklod:jan-klod-bundle-full
```

---

## Extension and configurator naming refinement

**User:**

```
extensions:
* provider-anthropic
* provider-openai
* provider-ollama
* manager-context
* store-memory
* registry-skills
* registry-mcp
* tool-web-search
* manager-agent-loop
* user-interface-terminal
* user-interface-graphical

Configurator UI - zip for config + core + extensions, like Spring or Quarkus
```

**Assistant:**

Group ID: `dev.janklod`

```
dev.janklod:jan-klod-core

dev.janklod:jan-klod-provider-anthropic    → implements LlmProvider
dev.janklod:jan-klod-provider-openai       → implements LlmProvider
dev.janklod:jan-klod-provider-ollama       → implements LlmProvider

dev.janklod:jan-klod-manager-context       → implements ContextManager
dev.janklod:jan-klod-manager-agent-loop    → implements AgentManager
                                             requires: LlmProvider, ContextManager
                                             optional: MemoryStore, SkillRegistry, McpRegistry

dev.janklod:jan-klod-store-memory          → implements MemoryStore

dev.janklod:jan-klod-registry-skills       → implements SkillRegistry
dev.janklod:jan-klod-registry-mcp          → implements McpRegistry

dev.janklod:jan-klod-tool-web-search       → consumes: AgentManager

dev.janklod:jan-klod-user-interface-terminal    → requires: AgentManager
dev.janklod:jan-klod-user-interface-graphical   → requires: AgentManager

dev.janklod:jan-klod-bundle-tui
dev.janklod:jan-klod-bundle-gui
dev.janklod:jan-klod-bundle-full
```

### ZIP layouts

**JVM:**
```
jan-klod.zip
├── jan-klod.yaml
├── lib/
│   ├── jan-klod-core-1.0.0.jar
│   └── ...selected extensions...
├── ext/
├── jan-klod
├── jan-klod.bat
└── README.md
```

**Bundle:**
```
jan-klod-tui-1.0.0-linux-amd64.zip
├── jan-klod
├── jan-klod.yaml
└── README.md
```

### Configurator UI

1. Version · 2. Packaging (jvm/tui/gui/full) · 3. LLM provider · 4. Extensions · 5. UI (terminal/graphical) → Generate & Download

---

## Rename packages and write BRAINSTORM.md

**User:** Rename `dev.janklod:jan-klod-core` → `com.github.janklod:core`; write `BRAINSTORM.md`.

**Assistant:** Generated. Group ID: `com.github.janklod`, drop `jan-klod-` prefix. Final map:

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

## How good is the Java stack for this job?

**Assistant:**

### Java strengths & weaknesses

**Strong:** Core container (registry, lifecycle, event bus), config (SnakeYAML + Jackson), reactive streaming (Mutiny + Vert.x), GUI (JavaFX/GluonFX), GraalVM native.

**Weak: TUI.**

| Library | Reality |
|---|---|
| Lanterna | Functional but dated |
| JLine | Line editing only |
| Blessed/Ink equivalent | Doesn't exist in Java |

Go/Rust/Python (Bubble Tea/Ratatui/Textual) far superior. Java SDK lags months behind TypeScript/Python. Startup: GraalVM ~50ms vs Go ~5ms (noticeable in CLI).

### Comparison

| Concern | Java/Quarkus | Go | Rust | Python | TypeScript |
|---|---|---|---|---|---|
| Core/extension system | ✅ Best | Good | Good | Poor | Poor |
| TUI | ❌ Weak | ✅ Best | ✅ Best | Good | Moderate |
| Native binary size | Moderate | ✅ Small | ✅ Smallest | ❌ N/A | ❌ N/A |
| MCP ecosystem | Lagging | Lagging | Lagging | ✅ Good | ✅ Best |
| Enterprise integration | ✅ Best | Moderate | Poor | Moderate | Poor |

### Pragmatic split

Java core/extensions, Go launcher/TUI. Launcher already separate for blue/green. Bubble Tea is best-in-class TUI. Java core as local server; TUI client connects (like LSP + editor).

---

## In case of splitting, will those Go parts change frequently?

**Assistant:**

### Launcher — almost never changes

Narrow job: download, health check, flip symlink or rollback. No domain knowledge. Forced changes are rare (once-a-year at most).

**Verdict: write it once, forget it.**

### TUI problem

Features (streaming, tools, MCP panels, skills, multi-agent) force constant TUI changes. Two languages = duplicate PRs/releases. **Go TUI changes frequently; split is costly.**

### Alternative

Java exposes web extension (localhost HTTP+SSE) → Tauri/Electron shell renders. Terminal UI = browser tab. One language, one release. Pattern: Zed, Warp, modern terminal apps.

### Revised split

| Component | Language | Change frequency |
|---|---|---|
| Launcher | Go | Rare — good fit |
| TUI/GUI shell | Tauri or Electron | Rare — thin wrapper only |
| UI rendering | HTML/CSS/JS via web extension | Frequent — stays in one stack |
| Everything else | Java | As designed |

---

## Go vs. Rust for launcher?

**Assistant:**

Both satisfy requirements (tiny binary, minimal deps, stable, process/symlink management, cross-platform). Go was default, worth questioning:

| | Go | Rust |
|---|---|---|
| Binary size | ~2MB | ~300KB |
| Startup | ~5ms | ~1ms |
| Memory | ~5MB | ~1MB |
| Cross-compilation | Trivial (`GOOS=windows go build`) | Good but heavier toolchain |
| Learning curve | Low | High |
| Safety | GC, no memory bugs | Borrow checker, no memory bugs |

Launcher code is simple; Rust's safety doesn't add much.

**Third option: shell + PowerShell.** Smaller, no binary, readable, cross-platform. Only Windows needed PowerShell; Go has no real advantage. Use Rust if your team knows it; else shell+PowerShell covers both with no new language.

---

## Artifacts

- `BRAINSTORM.md` — architecture & open questions
- `Handoff.md` — agent handoff
- `CONVERSATION.md` — this transcript

---

## Open questions (unresolved)

1. Multi-provider coexistence — one active LlmProvider or selectable per-request?
2. Extension versioning — independent or BOM-managed?
3. Streaming — in `manager-agent-loop` or a separate capability flag on `LlmProvider`?
4. MCP fault tolerance — how does `registry-mcp` handle server crashes?
5. Config hot-reload — react to YAML changes without restart?
6. JVM classloader isolation — per-extension or flat classpath?
7. Launcher language — Go vs Rust vs shell?
8. Configurator hosting — self-hosted only or public `start.janklod.dev`?
9. UI shell — Tauri vs Electron vs browser-only?
