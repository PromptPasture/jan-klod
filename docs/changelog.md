# Changelog

## 2026-06-28 (session 15)

- **Create**: First MVP code slice under `src/` — wazero host (`internal/host`), JSON-over-memory ABI (`internal/abi`), `jan-klod` host module with `host-log`, and `store-memory.wasm` guest extension. `make run` proves the host↔guest contract roundtrip end to end (set/get/list-keys/recent + host-log forwarding). Verified with `go vet`, golangci-lint v2 (0 issues), and `wasm-tools`.
- **Create**: Added `Makefile` (build/ext/run/test/lint/wit targets) and `.golangci.yml` (golangci-lint v2 config).
- **Create**: Added [decisions/2026-06-28-mvp-wasm-host/Handoff.md](decisions/2026-06-28-mvp-wasm-host/Handoff.md) — MVP findings: wazero has no component model → core-module JSON ABI; no CGo anywhere in MVP; SQLite runs host-side (not in the wasm guest), library choice deferred (`modernc/sqlite` vs `ncruces/go-sqlite3`).
- **Update**: [decisions/index.md](decisions/index.md) — added MVP WASM Host entry.

## 2026-06-28 (session 14)

- **Fix**: Reviewed and corrected all `wit/` interfaces — now pass `wasm-tools component wit wit/`. Changes: `http-error` enum→`variant` (had payloaded cases); `list`→`list-keys` (reserved keyword) in `memory-store`/`host-storage`; same-package refs switched from fully-qualified `jan-klod:interfaces/x@0.1.0` to short form (was self-cycle); `completion-chunk` case `tool-call`→`tool-call-request` (name clash with record).
- **Create**: Added `wit/types.wit` (`llm-types`, `store-types`) — single source for cross-interface records, decoupling `context-manager` from `llm-provider` and `host-storage` from `memory-store`.
- **Create**: Added `wit/extension-lifecycle.wit` — now exported by every extension world; defined the previously-undefined `extension-context`.
- **Update**: `agent-manager-world` now imports `tool-callable` and `agent-delegate` (orchestrator could not reach them before).
- **Update**: Replaced non-standard `wit/wit.toml` with `wit/README.md` (toolchain ignored the manifest).
- **Update**: Revised [concepts/contracts.md](concepts/contracts.md) — shared interfaces section; poll-based streaming sketch; lifecycle and world examples corrected to validated form.

## 2026-06-28 (session 13)

- **Create**: Added `wit/` — 13 WIT interface files under `jan-klod:interfaces@0.1.0`; 8 extension-exported (`llm-provider`, `context-manager`, `agent-manager`, `memory-store`, `skill-registry`, `mcp-registry`, `agent-delegate`, `tool-callable`) + 5 host-provided (`host-http`, `host-log`, `host-config`, `host-event`, `host-storage`).
- **Update**: Revised [concepts/contracts.md](concepts/contracts.md) — full interface table with file references; world structure example updated to match real `.wit` files; removed "not yet written" status note.

## 2026-06-28 (session 12)

- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — task routing with 10 built-in types + user-defined extension; parallel decomposition for file-edit, web-search, research, code-review.

## 2026-06-28 (session 11)

- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — two-level provider fallback (model within provider → next provider → error); handles rate limits, context overflow, local OOM, cost routing.

## 2026-06-28 (session 10)

- **Update**: Revised [concepts/architecture.md](concepts/architecture.md) — KISS/YAGNI as standing design rules; provider fallback with priority list; MCP fault tolerance (mark down, remove tools, backoff reconnect, UI warning).
- **Update**: Revised [decisions/2026-06-28-go-wasm-stack/Handoff.md](decisions/2026-06-28-go-wasm-stack/Handoff.md) — resolved last open question (MCP fault tolerance). All 8 open questions now closed.

## 2026-06-28 (session 9)

- **Update**: Revised [concepts/small-model-harness.md](concepts/small-model-harness.md) — layered router design: tier-1 heuristics (~50 grouped rules, microseconds) + tier-2 LLM classifier (one constrained token, reuses active provider, no embedding model).

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
