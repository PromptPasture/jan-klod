# Jan-Klod 🥷

> Minimal, stable AI agent core that does the splits between extensions.

## What it does

Jan-Klod is a Java/Quarkus AI agent runtime inspired by the Linux kernel philosophy: the core is small, versioned, and almost never changes. Everything domain-specific — LLM providers, memory, context management, tools, UI — lives in extensions that can be added, swapped, or disabled independently.

A core-only boot starts up and does nothing. That is intentional.

## Architecture

### Core responsibilities

| Responsibility     | Description                                              |
|--------------------|----------------------------------------------------------|
| Config loader      | Parses `jan-klod.yaml` into a typed config tree          |
| Extension registry | Discovers extensions, resolves dependency graph, orders boot |
| Lifecycle manager  | Drives `init → start → stop` and health checks           |
| Event bus          | Extension-to-extension communication                     |

### Extensions

Named `{role}-{name}`. Role prefix encodes type:

| Prefix              | Role                    |
|---------------------|-------------------------|
| `provider-*`        | LLM API clients         |
| `manager-*`         | Stateful orchestrators  |
| `store-*`           | Persistence backends    |
| `registry-*`        | Capability catalogues   |
| `tool-*`            | Callable tools          |
| `user-interface-*`  | Human-facing surfaces   |
| `bundle-*`          | Pre-packaged distributions |

## Getting started

### Prerequisites

- Java 21+
- GraalVM (for native builds)

### Installation

Download a pre-built bundle from the configurator or releases page, then:

```bash
# Extract and run
unzip jan-klod-tui-{version}-linux-amd64.zip
cd jan-klod-tui-{version}-linux-amd64
./jan-klod
```

For JVM mode, drop additional extension JARs into `ext/`.

## Configuration

`jan-klod.yaml` — YAML only. Set an extension to `false` to disable it; provide a mapping to configure it.

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

  user-interface-terminal:
    theme: dark

  user-interface-graphical: false
```

Skills follow the `.agents/skills/{skill-name}/SKILL.md` convention.

## Bundles

Pre-built native binaries for common configurations. Active extensions are still controlled by `jan-klod.yaml`.

| Bundle        | Contents                                                              |
|---------------|-----------------------------------------------------------------------|
| `bundle-tui`  | All providers + managers + store + registries + tools + terminal UI  |
| `bundle-gui`  | Same, with graphical UI instead                                       |
| `bundle-full` | Everything                                                            |
| *(jvm)*       | Core JAR only; drop extension JARs into `ext/`                       |

## Configurator

A Spring-Initializr-style web UI generates a ready-to-run ZIP. Select your version, packaging, LLM providers, and extensions — download and go.

## Updates & rollback

A tiny launcher binary manages blue/green version lifecycle:

```bash
# Manual rollback if needed
jan-klod rollback
```

Versions live in `~/.jan-klod/versions/`. The launcher health-checks each update and auto-rolls back on failure.

## Artifacts

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

## Contributing

See the org-wide [CONTRIBUTING.md](https://github.com/PromptPasture/.github/blob/main/CONTRIBUTING.md).

## License

Apache-2.0 — see [LICENSE](LICENSE).
