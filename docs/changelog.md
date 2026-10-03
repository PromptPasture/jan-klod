# Changelog

## 2026-10-03

- **Refactor**: **Phase 32 — `src/` becomes `pkgs/`** ([#274](https://github.com/PromptPasture/jan-klod/issues/274)). Packages are grouped `pkgs/host/*`, `pkgs/extensions/*` and `pkgs/clients/*`; the repo-root `Cargo.toml` is the host workspace, the guests and the Tauri shell keep their own. Every package has a README, and three package-specific wiki pages moved into their package's `docs/`. `make lints-drift` keeps the three `[workspace.lints]` blocks identical. Paths in this changelog's earlier entries are as they were when written.

## 2026-09-20

- **Implement**: **Slice 28e — Telegram sender allowlist: the sender id the Bot API asserts becomes the principal `telegram:<sender_id>`** (#257). `headless-chat` ships `telegram.allowed-senders: []` empty by default, refusing all senders until an operator explicitly lists them. Before a session is created, an unlisted sender is refused with a log message. The principal is passed through `run_with_driver_principal` to the conductor for downstream access in the interceptor. Test: `host/tests/it/telegram.rs::telegram_sender_allowlist_refuses_unlisted_and_accepts_listed`.

- **Implement**: **Slice 28c — principal reaches interceptors: the optional `principal` field on `user-turn`** (#255). The WIT contract (`wit/interceptor.wit`) adds `principal: option<string>` to the `user-turn` record handed to guests at the `before-loop` phase. Established by slice 28a (the principal is resolved from the bearer token in the REST/WebSocket guard and inserted into request state), the value reaches the conductor's `run_turn` function, is passed to `build_initial_request`, and flows to the guest via the interceptor host mapping. A guest may read this to make data-driven decisions (route to specific models, log the principal, redact it) — all policy encoded in config or guest heuristics, never inside the host. The API_VERSION bumped from 0.3.0 to 0.4.0 to reflect the contract change.

- **Implement**: **Slice 31c — capability groups as data, selected and merged by union, for setup and the Configurator** (#270). `scripts/capabilities.yaml` defines named groups of extensions that work together: `reasoning` (required, core agent phases), `content-guard`, `provider-openai`, `provider-anthropic`, `file-tools`, `version-control`, `shell-access`, `web-fetch`, `plan-analyze`, and `ecosystem`. A `pending` section documents Phase 30 guests not yet built (`tool-memory`, `tool-web-search`, `interceptor-persona`, `agent` category). Groups compose by union; per-instance naming resolves conflicts when two groups enable the same extension with different config. Validation tests in `src/config/tests/capabilities_validation.rs` verify YAML well-formedness, member existence, and merge logic.

- **Implement**: **Slice 30g — first-party skills ship inside the distribution archive** (#267). `registry-skills` now merges skills from two sources in priority order: `.agents/skills/` (workspace level, user override) checked first, and `skills/` (distribution level, bundled fallback) checked second. `scripts/bundle.sh` copies skill files into archives. Each distribution (`coding`, `headless-chat`, `minimal`, `self-extend`) ships three first-party skills: `commit.md` (clear commit message guidance), `review.md` (code review checklist), and `plan.md` (task breakdown patterns). The test `registry_skills_loads_bundled_and_workspace_overrides` verifies bundled skills are loaded and workspace override behavior works correctly.

- **Implement**: **Slice 30c — interceptor-persona: personality as data, read from config at select-model** (#263). The extension reads a configured default persona at init time and injects it into each turn's context before model selection at the `select-model` phase.

## 2026-09-19
