# Changelog

## 2026-06-28 (session 8)

- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — `agent-*` category for ACP delegation; ACP bidirectional (client + server); config hot-reload; deployment targets table (desktop, ARM NAS, Docker, Kubernetes).
- **Update**: Revised [concepts/contracts.md](concepts/contracts.md) — streaming mandatory in `llm-provider`; WIT sketch with `completion-request` + `stream<completion-chunk>`; multi-provider routing; `agent-delegate` WIT interface; resolved blocking open questions.
- **Update**: Revised [concepts/configurator.md](concepts/configurator.md) — GitHub Pages as launch hosting.
- **Update**: Revised [decisions/2026-06-28-go-wasm-stack/Handoff.md](decisions/2026-06-28-go-wasm-stack/Handoff.md) — closed 7 of 8 open questions; one remaining (MCP fault tolerance).

## 2026-06-28 (session 7)

- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — full extension taxonomy table; `chat-*` category for Slack/Telegram/WhatsApp/Mattermost; `api-*` as pluggable native extensions; core as Go module; single `~/.jan-klod/` config folder.
- **Update**: Revised [decisions/2026-06-28-go-wasm-stack/Handoff.md](decisions/2026-06-28-go-wasm-stack/Handoff.md) — added API, chat, and core delivery decisions.

## 2026-06-28 (session 6)

- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — UI as native Go extensions (`ui-tui`, `ui-web`, `ui-gui`) implementing `UIProvider`; same extension conventions as WASM but compiled in.
- **Update**: Revised [concepts/contracts.md](concepts/contracts.md) — documented two contract classes: WIT interfaces (WASM) and native Go interfaces (UI only); added `UIProvider` sketch.
- **Update**: Revised [decisions/2026-06-28-go-wasm-stack/Handoff.md](decisions/2026-06-28-go-wasm-stack/Handoff.md) — UI decision updated to native Go extension model.

## 2026-06-28 (session 5)

- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — single binary with all three UI modes (TUI default, `--web` browser, `--gui` Wails native window); built with Wails.
- **Update**: Revised [concepts/configurator.md](concepts/configurator.md) — bundles differ only in extensions and default mode, not binary variant.
- **Update**: Revised [decisions/2026-06-28-go-wasm-stack/Handoff.md](decisions/2026-06-28-go-wasm-stack/Handoff.md) — UI decision consolidated to single binary.

## 2026-06-28 (session 4)

- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — two binary variants (`jan-klod` pure Go with TUI+web; `jan-klod-gui` Wails with native window+web); all four UI modes documented.
- **Update**: Revised [concepts/configurator.md](concepts/configurator.md) — bundle presets now map to binary variants.
- **Update**: Revised [decisions/2026-06-28-go-wasm-stack/Handoff.md](decisions/2026-06-28-go-wasm-stack/Handoff.md) — UI decision updated to reflect two-binary model.

## 2026-06-28 (session 3)

- **Create**: Added [decisions/2026-06-28-go-wasm-stack/Handoff.md](decisions/2026-06-28-go-wasm-stack/Handoff.md) — locked Go + Wazero + WASM stack; full decision path and migration table from Java/Quarkus.
- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — Go stack, Wazero WASM host, WIT extension model, Bubble Tea TUI, Wails GUI, sqlc + modernc/sqlite.
- **Update**: Revised [concepts/contracts.md](concepts/contracts.md) — WIT interface definitions replacing Java interfaces; updated open questions.
- **Update**: Revised [concepts/blue-green-deployment.md](concepts/blue-green-deployment.md) — Go binary as launcher, per-extension `.wasm` updates, extension version tracking in state.yaml.
- **Update**: Revised [concepts/configurator.md](concepts/configurator.md) — WASM-based ZIP layout, bundle presets clarified, registry as simple HTTP file server.
- **Update**: Revised [decisions/index.md](decisions/index.md) — added new decision entry; marked 2026-06-16 as superseded.

## 2026-06-28 (session 2)

- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — locked stack decisions: Gradle, dual extension loading (Quarkus + ServiceLoader), REST+SSE transport, SQLite/Postgres/Supabase storage tiers, Micrometer+OTel observability, QuarkusTest testing strategy.
- **Update**: Revised [concepts/contracts.md](concepts/contracts.md) — added MemoryStore implementations note, corrected Maven→Gradle reference.

## 2026-06-28

- **Create**: Added [concepts/architecture.md](concepts/architecture.md) — core architecture, extension model, agent loop.
- **Create**: Added [concepts/contracts.md](concepts/contracts.md) — stable Java interfaces between core and extensions.
- **Create**: Added [concepts/small-model-harness.md](concepts/small-model-harness.md) — small-LLM agent loop design principles.
- **Create**: Added [concepts/blue-green-deployment.md](concepts/blue-green-deployment.md) — zero-downtime update strategy.
- **Create**: Added [concepts/configurator.md](concepts/configurator.md) — Configurator web UI description.
- **Create**: Added index files for concepts/ and decisions/ directories.
- **Update**: Rewrote root index.md with directory map and project overview.
- **Update**: Trimmed README.md to a short pitch + link to docs/; moved all detail into the wiki.
