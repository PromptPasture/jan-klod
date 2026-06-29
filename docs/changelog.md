# Changelog

## 2026-06-29 (session 23)

- **Create**: Added [decisions/2026-06-29-extension-technologies/BRAINSTORM.md](decisions/2026-06-29-extension-technologies/BRAINSTORM.md) (+ folder `index.md`) — language policy for our **first-party** extensions. Per-extension best-fit, **gated by CM-toolchain maturity *and* supply-chain posture** (the build pipeline is not protected by the runtime sandbox; npm-style worms fire at build time). **Default: Go (TinyGo) or Rust**; TS/JS + Python **case-by-case** (decisive library + hygiene); **Kotlin/JVM + Java excluded for now** (Beta, `wit-bindgen` fork, WASI threading unresolved). Cross-cutting supply-chain controls recorded. Provisional near-term assignments: Phase 1–2 extensions in Go (TinyGo); net footprint Rust core + Go extensions. Listed in [decisions/index.md](decisions/index.md).
- **Update**: [concepts/roadmap.md](concepts/roadmap.md) — added a **Status tracker** (per-phase `not-started`/`in-progress`/`blocked`/`done` flags) so phases run as a resumable loop; added the built-extension language ground rule (default Go/Rust); set the now-decided Phase 1–2 extension language to **Go (TinyGo)** (was "ideally non-Rust").

## 2026-06-29 (session 22)

- **Create**: Added [concepts/roadmap.md](concepts/roadmap.md) — phased build plan from the Rust + Wasmtime + Component Model foundation decision to a shippable runtime. **Phase 1** walking skeleton + foundation gate (Slice 1a = the go/no-go gate: Rust+Wasmtime host loads one *thin* non-Rust component over CM + settle the async model; Slice 1b = build out core + port host caps + re-author `provider-openai`/`store-memory` as real `wit-bindgen` components to MVP parity); **Phase 2** agent loop (`manager-agent-loop` + `manager-context` + harness — extensions, non-Rust-capable); **Phase 3** persistence + `host-serve`/`api-rest`; **Phase 4** UI client + `host-socket`/`chat-telegram` + ACP; **Phase 5** Go launcher/updater + configurator/bundles. Cross-cutting observability/testing; just-in-time **(TBD)** library decisions. Linked from [concepts/index.md](concepts/index.md).
- **Update**: [concepts/roadmap.md](concepts/roadmap.md) — dissolved the standalone "Phase 0 validation spike" into **Phase 1 Slice 1a** (the same go/no-go gate, now done with real not throwaway code, front-loading the foundation risk on a thin stub). Clarified that the **agent loop is an extension and need not be Rust** — building it in a non-Rust language is the strongest polyglot proof.
- **Decision**: **`core` is Rust-only; extensions are polyglot by design — any `wit-bindgen` language (Go/TS/Kotlin/Rust/…), best-fit per extension (Rust allowed but not required or defaulted); the launcher/updater is a tiny Go binary.** Recorded as the ground rules atop the roadmap.
- **Reset**: Removed the Go + Wazero MVP source (`src/**`, the 3 Go extensions, the Go host) and its orphaned build outputs (`ext/*.wasm`, `bin/jan-klod`). **`wit/` (15 canonical contracts) is kept untouched**; MVP findings remain captured in the 2026-06-28 decision records. Trimmed the Go-only `Makefile` to its still-valid `wit`/`clean` targets (Cargo + guest-build targets arrive in Phase 1).

## 2026-06-29 (session 21)

- **Fix (wiki lint)**: Normalized frontmatter across all `decisions/` files to the wiki convention (`type`/`title`/`description`/`tags`/`created`/`updated`). `2026-06-26-small-model-harness/Handoff.md` had **no frontmatter** (now `type: decision`); the four `generated:`-only handoffs and `BRAINSTORM.md`'s `status:/last-updated:` scheme replaced; `CONVERSATION.md` gained `tags`/`updated`. Dates preserved.
- **Fix (wiki lint)**: De-orphaned two concept pages by adding the missing cross-links from [architecture.md](concepts/architecture.md) → [contracts.md](concepts/contracts.md) (interface definitions) and → [configurator.md](concepts/configurator.md) (archive generation).
- **Create**: Added [decisions/2026-06-16-jan-klod/index.md](decisions/2026-06-16-jan-klod/index.md) cataloging that folder's three files (Handoff, Brainstorm, Conversation).
- **Note**: Left intentionally — the centralized root `changelog.md` (no per-directory changelogs) and the `Handoff.md`/`BRAINSTORM.md` filename pattern (folder name carries the title); single-file decision folders keep no `index.md` (parent `decisions/index.md` + filesystem scan suffice).

## 2026-06-29 (session 20)

- **Decision**: Settled runtime topology & trust after re-grounding on the exported original brainstorm ([decisions/2026-06-16-jan-klod/CONVERSATION.md](decisions/2026-06-16-jan-klod/CONVERSATION.md)). **Nothing is trusted** → every extension is a sandboxed WASM component; **the native/in-core extension tier is removed** (it was a Go-era conflation of "needs OS access" with "compiled into core"). **`core` runs as a standalone process under the user's privileges**, headless-capable, as the deploy unit. **Agent loop stays an extension** (`manager-agent-loop`) — Option A, confirmed from the brainstorm. **`api-*`/`chat-*` become ordinary sandboxed WASM extensions** reaching the network via planned `host-serve`/`host-socket` capabilities. **UIs are not extensions** — optional separate client processes connecting over an `api-*` HTTP+SSE surface (LSP model); one client binary, TUI/GUI by launch mode.
- **Update**: [decisions/2026-06-29-component-model-rust/Handoff.md](decisions/2026-06-29-component-model-rust/Handoff.md) — added "Runtime topology & trust" section; added open questions for `host-serve`/`host-socket` design and UI↔core transport.
- **Update**: [concepts/architecture.md](concepts/architecture.md) — core as standalone user-privilege process; dropped native tier; `api-*`/`chat-*` reclassified to WASM with host capabilities; `ui-*` removed from the taxonomy and rewritten as separate "User interfaces (separate clients)"; transport now an `api-*` extension; stack table gains a process-model row; deployment/Pi note clarified.
- **Update**: [concepts/contracts.md](concepts/contracts.md) — WIT is now the only extension contract (native-trait class removed); added planned `host-serve`/`host-socket` host interfaces; replaced the `UIProvider` section with a "UI ↔ core (client surface)" note.
- **Update**: [concepts/blue-green-deployment.md](concepts/blue-green-deployment.md) and [concepts/configurator.md](concepts/configurator.md) — deploy unit = core + `.wasm`; UI client is a separate, optional artifact; bundle table reframed around the separate UI client.
- **Move**: Relocated the exported brainstorm transcript from the repo root into [decisions/2026-06-16-jan-klod/CONVERSATION.md](decisions/2026-06-16-jan-klod/CONVERSATION.md) — added `type: source` frontmatter + cross-links; it is the primary source behind that folder's Handoff/BRAINSTORM. Linked it from [Handoff.md](decisions/2026-06-16-jan-klod/Handoff.md).

## 2026-06-29 (session 19)

- **Create**: Added [decisions/2026-06-29-component-model-rust/Handoff.md](decisions/2026-06-29-component-model-rust/Handoff.md) — foundation decision: untrusted extensions ⇒ in-process WASM sandbox; adopt the WebAssembly **Component Model**; host moves **Go → Rust + Wasmtime** (no mature pure-Go CM host; `wasmtime-go`+CGo rejected as "the trap"); earlier Rust rejection consciously reversed. WIT contracts, taxonomy, agent loop carry forward.
- **Update**: [decisions/index.md](decisions/index.md) — added the Rust + Wasmtime entry on top; marked 2026-06-28 MVP WASM Host superseded.
- **Fix**: Corrected the **TinyGo conflation** across docs — TinyGo is the small standalone **updater/supervisor** (stages, blue/green flip, rollback; separate process from the Rust core so it survives a core swap), *not* a guest-language proxy and not part of the host. Reframed the validation spike to "one **non-Rust** guest" confirming `wit-bindgen` polyglot authoring.
- **Update**: Recorded **polyglot extensions** as a first-class property (any `wit-bindgen` language per extension) in the decision record and [concepts/architecture.md](concepts/architecture.md) / [concepts/contracts.md](concepts/contracts.md).
- **Update**: Aligned all concept docs to the Rust + Wasmtime stack — [architecture.md](concepts/architecture.md) (stack table, Wazero→Wasmtime, native-Go→native-in-core Rust, testing, deployment), [contracts.md](concepts/contracts.md) (native Rust `UIProvider` trait, streaming toolchain note), [blue-green-deployment.md](concepts/blue-green-deployment.md) (TinyGo supervisor), [configurator.md](concepts/configurator.md) (Cargo build), [small-model-harness.md](concepts/small-model-harness.md) (`whatlang`), plus root [index.md](index.md) (was still "Quarkus + GraalVM" from the Java era) and [concepts/index.md](concepts/index.md) ("Java"→WIT). Open Rust library choices (SQLite, HTTP, SQL, TUI/WebView) marked **(TBD)** rather than invented.

## 2026-06-29 (session 18)

- **Create**: MVP slice 3 — `provider-openai` extension (`src/extensions/provider-openai/`), an OpenAI-compatible `llm-provider` (works against OpenAI, Ollama, vLLM, LM Studio via `base-url`). Reads `base-url`/`api-key`/`model` from host-config at start (start fails if `base-url` unset); `complete` op builds a non-streaming `/chat/completions` request, calls host-http, parses `choices[0].message.content` + `finish_reason`; `info` op returns id + configured model. HTTP status mapped to `provider-error` (401/403→auth-failed, 404→model-not-found, 429→rate-limited, else transient). Streaming and tool-calling deferred.
- **Create**: Added `internal/host/provider_openai_test.go` — mock `/chat/completions` (httptest): completion happy path (asserts non-streaming, bearer auth, config default model), 401→auth-failed, start-without-base-url failure.
- **Update**: `Makefile` ext list now builds provider-openai.wasm.
- **Verify**: `go test ./...` pass, `go vet` clean, golangci-lint 0 issues.

## 2026-06-28 (session 17)

- **Create**: MVP slice 2 — wired `host-config` and `host-http` into the `jan-klod` host module (`internal/host/hostfuncs.go`). `config_get` serves each extension its own config section (identified by `m.Name()`), value returned JSON-encoded per `wit/host-config.wit`. `http_fetch` performs sandboxed outbound HTTP via host `net/http`; completed exchanges (incl. 4xx/5xx) return ok+status+body so callers can read API error payloads, transport failures return ok=false (ABI encoding of `wit/host-http.wit`). Bodies base64-encoded; host-returned data allocated in guest memory via its `alloc` export (`returnJSON` helper).
- **Create**: Added `probe-host` test-fixture extension (`src/extensions/probe-host/`) that exercises host-log/host-config/host-http over the ABI; not shipped.
- **Create**: Added `internal/host/host_test.go` — integration tests (httptest, no external network): config get, missing-key error, HTTP POST body round-trip, connection-refused.
- **Update**: `Makefile` — pattern rule builds any `extensions/%`; `ext` builds store-memory + probe-host; `test` depends on `ext`.
- **Verify**: `go test ./...` (config + host packages) pass, `go vet` clean, golangci-lint 0 issues.

## 2026-06-28 (session 16)

- **Create**: MVP slice 1 — config-driven extension loader. Added `internal/config` (parses `jan-klod.yaml`: per-extension sections, `enabled` bool/map shorthand, `${ENV}` expansion) and `internal/host/loader.go` (`LoadConfigured` scans `ext/`, loads enabled extensions, runs lifecycle init→start→health, registry-tracked). Lifecycle (`init`/`start`/`stop`/`health` from `wit/extension-lifecycle.wit`) routed through the existing `invoke` ABI via reserved `lifecycle.*` ops, keeping the guest export surface at alloc/free/invoke. `store-memory` guest now handles lifecycle ops; `Host.Close` stops all extensions.
- **Create**: Added root `jan-klod.yaml` (sample config: `store-memory` on, `provider-openai` declared but disabled until slice 3).
- **Create**: Added `internal/config/config_test.go` (bool/map shorthand, env expansion, default-enabled, missing-file).
- **Verify**: `make run` (provider-openai skipped as disabled, store-memory init→start→health=up→roundtrip→stop), `go test ./...` pass, `go vet` clean, golangci-lint 0 issues.

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
