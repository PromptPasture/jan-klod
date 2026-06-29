---
type: source
title: Jan-Klod — Original Brainstorm Conversation (verbatim)
description: Full transcript of the 2026-06-15 brainstorm that produced the 2026-06-16 Jan-Klod design. Java/Quarkus era — superseded by later decisions, kept as the primary source.
created: 2026-06-15
---

# Jan-Klod — Full Brainstorm Conversation

> Created: 2026-06-15 · **Primary source** behind this folder's distillations,
> [Handoff.md](Handoff.md) and [BRAINSTORM.md](BRAINSTORM.md). This is the raw
> transcript (Java/Quarkus era); the architecture has since moved to Rust +
> Wasmtime + the Component Model — see
> [2026-06-29-component-model-rust](../2026-06-29-component-model-rust/Handoff.md).
> Several ideas here are still load-bearing (Linux-kernel core, agent-loop as an
> extension "Option A", the LSP-style "core is a server, UI is a client" split,
> the tiny rarely-changing launcher).

---

## Is it possible to do Agent harness on Java? For example, Quarkus framework. What advantages and disadvantages will it have against common Claude Code, OpenCode and others?

**Assistant:**

### Java/Quarkus as an Agent Harness

Yes, absolutely viable — and there's already a solid foundation in **LangChain4j's Quarkus extension**. Here's an honest breakdown.

#### What a Quarkus agent harness looks like

Quarkus gives you:

- **`quarkus-langchain4j`** — declarative AI services, tool binding, memory, embeddings
- **Mutiny + Vert.x** — non-blocking streaming responses from the LLM API
- **MicroProfile REST Client** — clean Anthropic API integration
- **CDI** — dependency injection for skills, tools, and handlers
- **GraalVM native** — compiled binary with fast startup

A minimal agent loop in Quarkus looks like:

```java
@ApplicationScoped
@RegisterAiService(tools = { FileSystemTool.class, BashTool.class })
public interface AgentService {
    @SystemMessage("You are a coding agent. Use tools to complete tasks.")
    @UserMessage("{input}")
    Uni<String> run(String input);
}
```

Skills from `SKILL.md` would be loaded as system prompt segments at startup, injected via `@SystemMessage` composition or a custom `ChatMemory`.

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

**Where Quarkus wins:**

- **Production enterprise deployments** — auth, secrets management, DB-backed state, audit logs, multi-tenancy all have mature Quarkus solutions
- **Multi-agent orchestration as a service** — expose agents as REST/gRPC endpoints trivially
- **Observability** — built-in tracing means you can watch every tool call through Jaeger/Grafana out of the box
- **Horizontal scaling** — Kubernetes-native, container image sizes ~50MB native vs 500MB+ Python

**Where Quarkus loses:**

- **MCP protocol support** is TypeScript/Python-first; Java MCP SDKs lag behind
- **Dynamic skill loading** is painful with GraalVM native (reflection must be declared at build time — runtime-discovered `SKILL.md` files require the JVM mode or careful native config)
- **Community and skills ecosystem** — Claude Code and Python agents have far more pre-built integrations
- **Iteration speed** — prototyping an agent in Python or TS is meaningfully faster
- **Developer tooling** — Claude Code's editor integration, OpenCode's TUI are battle-tested; building equivalent UX in Java is significant work

#### The core architectural friction

The skills spec defines skills as **runtime-discovered Markdown folders**. This creates a specific tension with GraalVM native:

```
Skills loaded at runtime (filesystem scan)
       ↕ friction
GraalVM native (classpath fixed at build time)
```

Your options:

1. **Run in JVM mode** — no friction, full dynamic loading, slower startup (~1s), still fine for server deployments
2. **Bundle skills at build time** — fast native binary, but skills can't be user-installed at runtime without a restart
3. **Hybrid** — native binary with a watched directory for skills, reload on change (works but complicates the native reflection config)

#### When to choose Quarkus

**Good fit:**
- Agent deployed as a backend service (not a local CLI tool)
- Org already on Java/JVM stack
- Needs multi-tenancy, auth, audit, database-backed memory
- Running hundreds of concurrent agent sessions

**Poor fit:**
- Local developer agent (Claude Code's domain — TS is better here)
- Rapid prototyping or research
- Heavy reliance on Python ML libraries (embeddings, fine-tuning, evals)
- Need bleeding-edge MCP support

#### Practical recommendation

If you're building a **server-side agent platform** for an enterprise Java shop, Quarkus + LangChain4j is genuinely competitive. Start in JVM mode for skill flexibility, profile, then selectively native-compile once your skill set is stable.

If you're building a **local coding agent** to compete with Claude Code/OpenCode, Java is the wrong pick — the CLI/TUI ecosystem, startup expectations, and MCP tooling all favor TypeScript or Go.

---

## Usually skills are stored in .agents/skills folder

**Assistant:**

Good to know — that aligns with the `.agents/` convention already established in `AGENTS.md` for memory files. So the full layout would be:

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

This matters for the Quarkus harness discussion specifically — the agent startup routine would need to scan `.agents/skills/` at launch, walking subdirectories for `SKILL.md` files and loading their frontmatter for trigger matching. In JVM mode that's a straightforward `Files.walk()`. In GraalVM native you'd either pre-index the directory at build time or configure the native image to allow filesystem access to that path at runtime.

Do you want me to use `.agents/skills/` going forward when referencing or creating skill paths?

---

## Wait. We are brainstorming yet. How hard will it be to support extensions? I'm thinking on making something small as Pi Agent, just a core, like Linux, and expand it via extensions

**Assistant:**

Good framing. The Linux kernel analogy maps well — core handles the agent loop, context, and API communication; everything else is a module.

### What "extensions" would actually cover

Worth separating the layers before thinking about hardness:

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

**Skills — trivial.** Just filesystem scanning. Already solved by the `.agents/skills/` convention. No API surface needed.

**MCP servers — nearly free.** The protocol is already the extension boundary. Any language, any process, stdio or SSE. You just need a registry of which servers are active.

**Tools — moderate.** Java's `ServiceLoader` is the natural fit — drop a JAR, implement the interface, it's discovered. The hard part is defining a stable tool API that doesn't break extensions when core evolves.

**Core hooks (middleware, interceptors) — hard.** If extensions want to intercept the agent loop itself — pre/post tool calls, custom memory backends, alternate routing — you need a proper plugin lifecycle with defined hook points. This is where scope creep kills small projects.

### The real risk

The Linux kernel took decades to stabilize its module API. Your biggest threat is **premature API surface** — every hook point you expose becomes a contract.

A safer path: start with zero extension hooks in core, only skills + MCP as extension mechanisms, and let real usage pressure tell you where the next hook point needs to be.

### Java-specific friction

GraalVM native and runtime JAR loading don't mix well. If you want drop-in JAR extensions, you're in JVM mode. That's not necessarily bad — a small persistent agent process doesn't need sub-10ms startup — but it's worth deciding early.

### What's next

What's your instinct on the extension boundary — do you want extensions to be able to add **tools** (code), or stay at **skills + MCP** (configuration/protocol)? That choice drives everything else.

---

## I want to make a core + extensions: skills support, mcp support, memory, web,.. And make a configurator like Spring Boot or Quarkus has - user choose version, packaging (jar/native), set of packages (jar will come with, native will be from pre-built set). Also want to make it blue/green deployment on update, if something goes wrong, user will always have working instance

**Assistant:**

This is a genuinely ambitious architecture. Let me map out the shape of it and where the hard problems live.

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

### The combinatorial problem for native

This is the first hard decision. 10 extensions = 1,024 possible native binaries. Not viable to pre-build all.

**Three practical options:**

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

### Where the real complexity lives

**State migration is the silent killer.** Memory schemas, config formats, conversation history — every core upgrade needs a migration path. Design the state format for portability from day one.

**Extension API stability.** Every interface you expose to extension authors becomes a contract. Define the minimum viable API surface, freeze it early, version it explicitly.

**The configurator itself** is straightforward — it's essentially a form that generates a `pom.xml` or downloads a pre-built binary. Spring Initializr is open source; you could fork it.

### What to decide next

1. **Is this CLI, server, or both?**
2. **Native bundles or hybrid?**
3. **What's in core vs. extensions?**
4. **State format?** SQLite is the obvious choice.

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

### Full artifact map

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

### Configurator ZIP layouts

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

### Configurator UI flow

```
1. Version        [1.0.0 ▾]
2. Packaging      ( ) jvm  (•) tui  ( ) gui  ( ) full
3. LLM Provider   [x] Anthropic  [ ] OpenAI  [ ] Ollama
4. Extensions     [x] Context    [x] Memory  [x] Skills
                  [x] MCP        [ ] Web Search
5. UI             (•) Terminal   ( ) Graphical
                  ───────────────────────────
                  [ Generate & Download ]
```

---

## Rename packages and write BRAINSTORM.md

**User:** Rename packages `dev.janklod:jan-klod-core` => `com.github.janklod:core` and write draft `BRAINSTORM.md`

**Assistant:** *(Generated `BRAINSTORM.md` — see artifact. Key change: group ID is now `com.github.janklod`, artifact IDs drop the `jan-klod-` prefix.)*

Final artifact map:

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

### Where Java is strong

- **Core container** — excellent. Extension registry, lifecycle, event bus. This is exactly Java's domain.
- **Config and wiring** — solid. SnakeYAML + Jackson, typed binding.
- **Reactive streaming** — good. Mutiny + Vert.x handle LLM streaming and MCP stdio/SSE cleanly.
- **GUI** — acceptable. JavaFX / GluonFX with GraalVM native support.
- **GraalVM native for bundles** — works well when extension set is fixed at build time.

### Where Java is weak

**TUI — the real problem.**

| Library | Reality |
|---|---|
| Lanterna | Functional but dated |
| JLine | Line editing only |
| Blessed/Ink equivalent | Doesn't exist in Java |

Go has **Bubble Tea**. Rust has **Ratatui**. Python has **Textual**. All significantly better.

**MCP ecosystem** — Java SDK lags TypeScript and Python by months.

**Startup perception** — GraalVM native ~50ms vs Go ~5ms. Noticeable for interactive CLI use.

### Comparison

| Concern | Java/Quarkus | Go | Rust | Python | TypeScript |
|---|---|---|---|---|---|
| Core/extension system | ✅ Best | Good | Good | Poor | Poor |
| TUI | ❌ Weak | ✅ Best | ✅ Best | Good | Moderate |
| Native binary size | Moderate | ✅ Small | ✅ Smallest | ❌ N/A | ❌ N/A |
| MCP ecosystem | Lagging | Lagging | Lagging | ✅ Good | ✅ Best |
| Enterprise integration | ✅ Best | Moderate | Poor | Moderate | Poor |

### Pragmatic split considered

**Core and extensions in Java, launcher and TUI shell in Go.**

Launcher is already a separate binary for blue/green. Go's Bubble Tea TUI is best-in-class. Java core runs as a local server; TUI connects to it — similar to how LSP works with editors.

---

## In case of splitting, will those Go parts change frequently?

**Assistant:**

### Launcher — almost never changes

Narrow job: download, health check, flip symlink or rollback. No domain knowledge. Forced changes are rare (once-a-year at most).

**Verdict: write it once, forget it.**

### TUI — this is the problem

Every meaningful product feature forces a TUI change:
- Streaming token display
- Tool call visualisation
- MCP server status panel
- Skill browser
- Multi-agent support

Two languages = two PRs, two release artifacts to keep in sync. That friction compounds.

**Verdict: Go TUI will change frequently. The split is costly.**

### Alternative boundary

```
Java core exposes web extension (localhost HTTP + SSE)
          ↓
Browser or thin Tauri/Electron shell renders the UI
```

- `user-interface-terminal` opens a browser to `localhost:port`
- `user-interface-graphical` is the same backend, polished Tauri wrapper
- New features ship in Java only — one language, one release
- Same pattern used by Zed, Warp, and most modern "terminal" apps

### Revised split recommendation

| Component | Language | Change frequency |
|---|---|---|
| Launcher | Go | Rare — good fit |
| TUI/GUI shell | Tauri or Electron | Rare — thin wrapper only |
| UI rendering | HTML/CSS/JS via web extension | Frequent — stays in one stack |
| Everything else | Java | As designed |

---

## Why Go specifically — is it better than Rust?

**Assistant:**

Not specifically — Go was a default assumption worth questioning.

For the launcher, both satisfy all requirements (tiny binary, minimal deps, rarely changes, process/symlink management, cross-platform):

| | Go | Rust |
|---|---|---|
| Binary size | ~2MB | ~300KB |
| Startup | ~5ms | ~1ms |
| Memory | ~5MB | ~1MB |
| Cross-compilation | Trivial (`GOOS=windows go build`) | Good but heavier toolchain |
| Learning curve | Low | High |
| Safety | GC, no memory bugs | Borrow checker, no memory bugs |

For a launcher that flips symlinks and runs health checks, Rust's safety guarantees don't buy much — the code is simple enough in either language.

**Third option: shell script + PowerShell.**

The launcher is essentially:
```bash
download new version
start new binary
curl health check → pass/fail
ln -sfn current or rollback
```

A well-written shell script with no dependencies is smaller than any compiled binary, works everywhere, and is readable by anyone. The only real argument against it is Windows compatibility — which PowerShell covers.

**Bottom line:** Go has no meaningful advantage over Rust here. Defaulting to Go was habit, not reasoning. Use Rust if your team knows it. If neither, shell + PowerShell covers both platforms with no new language required.

---

## Artifacts produced in this session

- `BRAINSTORM.md` — full architecture decisions and open questions
- `handoff-jan-klod.md` — agent handoff document
- `CONVERSATION.md` (originally `jan-klod-conversation.md`) — this file

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
