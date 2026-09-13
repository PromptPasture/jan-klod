---
type: plan
title: Shipping Plan — Jan-Klod v0.1.0
description: Four phases to turn the completed internal build (Phases 1–8) into a product a developer can install and use daily.
tags: [plan, shipping, v0.1.0, release]
created: 2026-07-03
updated: 2026-07-03
---

# Shipping Plan — Jan-Klod v0.1.0

## Context

Phases 1–8 done + gated. Component Model host, v1 interceptors, persistence, REST, TUI, streaming, file-workspace, tools all proven end-to-end. Gap to daily-usable agent: not architecture — functional + distribution layers.

## Comparison baseline

Reviewed vs. [earendil-works/pi](https://github.com/earendil-works/pi) (2026-07-03). Pi: production, multi-provider, rich TUI, skills, TypeScript extensions (no boundary). Jan-Klod differentiator: WASM Component Model boundaries give per-extension sandboxing, typed WIT contracts, fail-closed permission gate — no container.

## Blockers for v0.1.0

| # | Item | Why |
|---|---|---|
| 9 | Anthropic provider | Most users run Claude; OpenAI proxy loses native streaming + extended thinking. |
| 10 | Skills + MCP | No named workflows or ecosystem tools → narrower than Pi out-of-box. |
| 11 | UX polish | Per-token TUI streaming (Phase 6 carry-forward); session resume daily-use requirement. |
| 12 | Release | GitHub releases + Pages + install + quickstart = how users get jan-klod. |

---

## Phase 9 — Anthropic provider

**Goal:** Claude natively, no OpenAI-compat proxy.

- **`provider-anthropic`** — Rust guest, `llm-provider`, calls Messages API via `host-http`. Handles: Anthropic SSE → `completion-chunk`s; `tool_use` → `tool-call-request`; status codes; extended-thinking flag. `init` reads `api-key`/`model` from `host-config` (no key logging). One wasm for all models.
- **Config** — `extensions.provider.anthropic: {enabled: true, api-key: ${ANTHROPIC_API_KEY}, model: claude-sonnet-4-6}`. Model config, not code.
- **Supply-chain** — same audit/deny path as `provider-openai`.

**Done:** enable provider in config, run `make probe` with live `ANTHROPIC_API_KEY` → real completion through guest (tokens cost). Offline: canned reply → correct chunks.

**Success:** user sets `ANTHROPIC_API_KEY`, enables `provider.anthropic`, runs `jan-klod serve` against Claude, no proxy.

---

## Phase 10 — Skills + MCP registry

**Goal:** named workflow shortcuts + ecosystem tool access → broadly useful agent.

### Skills (`registry-skills`)

- `registry-skills` implements `skill-registry` WIT. Scans `.agents/skills/` for Markdown with YAML `name:`, exposes `list-skills`/`get-skill`/`invoke`/`reload`. `ToolFleet` calls `list-skills` at `select-tools`, `invoke` at dispatch. No new WIT.

### MCP gateway (`registry-mcp`)

- `registry-mcp` implements `mcp-registry` WIT. Connects to configured MCP servers, exposes `list-tools`/`invoke-tool`, handles reconnect via `host-event`. **v0.1.0: SSE only** (via `host-http`). Stdio deferred (needs long-lived children). Config: `extensions.registry.mcp: {enabled: true, servers: [{name: "fs", transport: "sse", url: "http://localhost:3000/sse"}]}`. Multiple servers use prefix (`fs::read_file`). Permission gates each call; results untrusted. Prerequisite: verify `host-event` grant.

**Done:** model calls MCP tool through registry with canned SSE stub (offline). Model invokes skill from `.agents/skills/review.md`, template injected.

---

## Phase 11 — UX polish

**Goal:** daily-use experience matches developer expectations.

### Per-token streaming in TUI (Phase 6 carry-forward)

Conductor emits `text-delta` via `EventSink`, REST streams SSE. TUI currently waits for `done`. Fix: consume `text-delta` in TUI event loop, append to buffer, re-render. Wiring only.

### REST API surface (2026-07-03)

Replace `POST /turn` with resource model: 
```
GET  /health                    liveness probe
GET  /sessions                  list {id, created, preview}
POST /sessions                  create → {id}
GET  /session/:id               transcript + metadata
POST /session/:id/message       send, stream SSE
```
No `PUT`/`DELETE` v0.1.0 — append-only, pruning post-v0.1.0. Contained to `serve.rs` + client.

### Session resume

- `GET /sessions` returns past sessions from store.
- `jan-klod-ui --session <id>` fetches list, pick to resume.
- `POST /session/:id/message` with existing id resumes from transcript; conductor replays history before first turn.

### Workspace auto-detect

Launch `jan-klod serve` without `workspace:` config → default to `$PWD`. `Runtime::boot` config change.

**Done:** `jan-klod serve` in repo → send message → disconnect → relaunch → resume by id → per-token streaming, all in TUI.

---

## Phase 12 — Release: GitHub + web

**Goal:** developer finds jan-klod, installs, working session in 15 minutes.

### GitHub releases

`make bundle` → `dist/jan-klod-<version>-<os>-<arch>.tar.gz`. GitHub Actions on `git tag v*`: build matrix (linux-{amd64,arm64}, darwin-{arm64,amd64}), attach bundles, publish with changelog. Release asset is install unit; no registry needed.

### Install script

`scripts/install.sh`: `curl -sSL https://.../install.sh | sh` detects OS/arch, downloads bundle from latest release, verifies checksum, extracts to `~/.local/bin/jan-klod`. Stdlib sh, no deps.

### GitHub Pages

Static site `pages/`, served from main. Content: what it is (1¶), differentiator (WASM sandboxing), install cmd, quickstart link, repo link. Plain HTML + CSS or single-file Hugo/Jekyll. Fast, easy.

### Quickstart

Install → set API key → `jan-klod serve` → first session in TUI → fix bug in repo. Golden path, <500 words.

### README

Replace stub. Sections: what it is (1 sentence), why different from Pi (WASM sandboxing), install, quickstart link, TUI screenshot, docs link.

**Done:** unfamiliar person follows README install, completes quickstart, model reads/edits real repo file.

---

## Sequencing

```
Phase 9  (Anthropic)        ──┐
Phase 10 (Skills+MCP)       ──┼── Phase 11b (session resume, workspace) ──┐
Phase 11a (streaming — now) ──┘                                            ├── Phase 12 (Release)
                                                                           ┘
```

Phase 11a (per-token streaming in TUI) has no dependency on 9 or 10 and can start
immediately. Phases 9 and 10 are independent of each other and run in parallel.
Phase 11b (session resume, workspace auto-detection) waits for 9 and 10 so the full
experience is polished together. Phase 12 follows when all three are complete.

---

## Completion criteria (v0.1.0)

Jan-Klod v0.1.0 ships when:

1. A developer installs it on macOS or Linux with a one-line curl command.
2. They set `ANTHROPIC_API_KEY` (or any OpenAI-compat key), run `jan-klod serve` in a
   repo, and the model reads files, proposes edits, runs tests, and reports back —
   with per-token streaming in the TUI.
3. The permission gate asks before any destructive shell command; the user can approve
   or block.
4. Sessions persist and are resumable by id.
5. The GitHub release page exists with versioned bundle assets.
6. The GitHub Pages site explains what Jan-Klod is and links to the install command.
