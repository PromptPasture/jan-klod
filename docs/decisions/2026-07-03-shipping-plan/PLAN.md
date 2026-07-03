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

Phases 1–8 are all done and gated in CI. The WASM Component Model host, full v1
interceptor set, persistence, REST surface, TUI client, streaming, file-workspace
substrate, and tool fleet (`tool-fs` + `tool-shell`) are all proven end-to-end. The
gap to a daily-usable coding agent is not architectural — it is the functional and
distribution layer a user actually touches.

## Comparison baseline

Reviewed against [earendil-works/pi](https://github.com/earendil-works/pi) (2026-07-03).
Pi is the production reference: shipping product, multi-provider, rich TUI, skills,
TypeScript extension system with no permission boundary. Jan-Klod's differentiator is
structural: WASM Component Model boundaries give per-extension sandboxing, typed WIT
contracts, and a fail-closed permission gate — without requiring a container.

## What blocks v0.1.0

| # | Item | Blocking because |
|---|---|---|
| 9 | Anthropic provider | Most target users run Claude; OpenAI proxy works but loses native streaming + extended thinking |
| 10 | Skills + MCP registry | Without named workflows or ecosystem tool access, the agent is narrower than Pi out of the box |
| 11 | UX polish | Per-token streaming in TUI is a Phase 6 carry-forward; session resume is a daily-use requirement |
| 12 | Release layer | GitHub releases + Pages + install script + quickstart = how users actually get and try Jan-Klod |

---

## Phase 9 — Anthropic provider

**Goal:** Claude works natively without an OpenAI-compat proxy.

- **`provider-anthropic` extension** — Rust guest implementing `llm-provider`. Calls
  the Anthropic Messages API (`/v1/messages`) directly via `host-http`. Handles:
  streaming SSE response from Anthropic's format → `completion-chunk` sequence the
  conductor expects; native tool-call blocks (`tool_use` content) → `tool-call-request`
  chunks; `401/403` → `auth-failed`, `429` → `rate-limited`, else `transient`.
  `init` reads `api-key` + `model` from `host-config` (never logs the key).
  Extended thinking passthrough via `host-config` flag.
- **Config entry** — `extensions.provider.anthropic: {enabled: true, api-key:
  ${ANTHROPIC_API_KEY}, model: claude-sonnet-4-6}` in `config.yaml`. One wasm for
  all Anthropic models (model is config, not code).
- **Supply-chain** — same `cargo-audit`/`cargo-deny` path as `provider-openai`.

**Exit gate:** enable `provider.anthropic` in `config.yaml` and run `make probe` with
a live `ANTHROPIC_API_KEY` — drives a real completion through the sandboxed guest
(token-costing). Offline: canned `host-http` reply → correct chunk sequence. (`make
probe` runs against whichever provider is enabled in config; no `PROVIDER=` flag.)

**Success condition:** a user can set `ANTHROPIC_API_KEY`, enable `provider.anthropic`
in `config.yaml`, and run `jan-klod serve` against Claude with no proxy.

---

## Phase 10 — Skills + MCP registry

**Goal:** named workflow shortcuts and ecosystem tool access — the two features that
make a coding agent broadly useful vs narrowly capable.

### Skills registry (`registry-skills`)

- A **`registry-skills` extension** implements the `skill-registry` WIT interface
  (`wit/skill-registry.wit`). It scans `.agents/skills/` in the workspace for Markdown
  files with a YAML front matter `name:` field and exposes them via `list-skills`,
  `get-skill`, `invoke`, and `reload` (hot-reload on config change).
- The `ToolFleet` integrates skills by calling `list-skills` at `select-tools` time
  (to advertise them to the model) and `invoke` at dispatch time — the same seam used
  for `tool-callable`, but through the `skill-registry` interface.
- No new WIT interface: `skill-registry.wit` already exists and is canonical.

### MCP gateway (`registry-mcp`)

- A **`registry-mcp` extension** implements the `mcp-registry` WIT interface
  (`wit/mcp-registry.wit`). It connects to configured MCP servers, exposes
  `list-tools` and `invoke-tool`, and handles reconnection via `host-event`
  notifications.
- **v0.1.0 scope: SSE transport only** (via `host-http`). Stdio transport requires
  long-lived child process support (`host-process` carry-forward from Phase 7) and
  is deferred post-v0.1.0.
- Config: `extensions.registry.mcp: {enabled: true, servers: [{name: "fs", transport:
  "sse", url: "http://localhost:3000/sse"}]}`. Multiple servers use a prefixed
  namespace (`fs::read_file`).
- The `ToolFleet` calls `list-tools` at `select-tools` time and `invoke-tool` at
  dispatch. Permission gate fires on each outbound MCP call; results are untrusted.
- **Prerequisite:** verify `host-event` is granted to `mcp-registry-world` guests
  before implementation starts (the WIT world imports it for crash/reconnect events).

**Exit gate:** model calls an MCP tool through the registry with a canned SSE stub
(offline). Model invokes a skill from `.agents/skills/review.md` and the template is
injected correctly.

---

## Phase 11 — UX polish

**Goal:** the daily-use experience matches what a developer expects from a coding agent.

### Per-token streaming in TUI (Phase 6 carry-forward)

The conductor already emits `text-delta` events via the `EventSink` and the REST
surface streams them as SSE. `jan-klod-ui`'s `ratatui` TUI currently waits for `done`
before rendering. Fix: consume `text-delta` events in the TUI's event loop and append
to the active message buffer on each event, triggering a re-render. No architectural
change — wiring only.

### REST API surface (decided 2026-07-03)

The v1 REST surface is a proper resource model. `POST /turn` (Phase 3, session id
in body) is replaced before v0.1.0 ships — the change is contained to `serve.rs`
and the UI client.

```
GET  /health                    liveness (blue/green supervisor probe)
GET  /sessions                  list sessions {id, created, preview}
POST /sessions                  create session → {id}
GET  /session/:id               transcript + metadata
POST /session/:id/message       send message, stream SSE response
```

No `PUT` or `DELETE` in v0.1.0 — sessions are append-only and pruning is post-v0.1.0.

### Session list and resume

- `GET /sessions` returns past sessions from the SQLite store.
- `jan-klod-ui --session <id>` (or `/sessions` REPL command) fetches the list and
  lets the user pick a session to resume.
- `POST /session/:id/message` with an existing id resumes from that session's
  transcript. The conductor replays stored history into the context interceptor
  before the first new turn.

### Workspace auto-detection

When `jan-klod serve` is launched without an explicit `workspace:` config key, default
the workspace root to `$PWD`. The `host-fs` and `host-process` substrates already
accept a runtime workspace path; this is a config-defaulting change in `Runtime::boot`.

**Exit gate:** start `jan-klod serve` in a repo, send a message, disconnect, relaunch,
resume the session by id, and receive per-token streaming output — all in the TUI.

---

## Phase 12 — Release: GitHub + web

**Goal:** a developer who has never heard of Jan-Klod can find it, install it, and
have a working session within 15 minutes.

### GitHub releases

- `make bundle` already produces `dist/jan-klod-<version>-<os>-<arch>.tar.gz`. Wire
  this into a GitHub Actions release workflow: on `git tag v*`, build the matrix
  (linux-amd64, linux-arm64, darwin-arm64, darwin-amd64), attach bundles as release
  assets, and publish the GitHub release with a generated changelog section.
- The release asset is the install unit; no separate package registry needed for v0.1.0.

### Install script

- `scripts/install.sh`: `curl -sSL https://raw.githubusercontent.com/.../install.sh | sh`
  detects OS/arch, downloads the matching bundle from the latest GitHub release, verifies
  the checksum, extracts to `~/.local/bin/jan-klod` (or `/usr/local/bin` with sudo).
- One page of stdlib sh; no dependencies.

### GitHub Pages site (`pages/`)

- Static site under `pages/` in the repo, served via GitHub Pages from the `main`
  branch `pages/` directory.
- Content: what Jan-Klod is (one paragraph), the differentiator (WASM sandboxing),
  install command, link to quickstart, link to the GitHub repo. No framework — plain
  HTML + minimal CSS, or a single-file Hugo/Jekyll layout. Fast to load, easy to update.

### Quickstart doc (`docs/quickstart.md`)

- Install → set API key → `jan-klod serve` → first session in the TUI → point at a
  repo and ask it to fix a bug. Golden path only. Under 500 words.

### README rewrite

- Replace the current stub. Sections: what it is (one sentence), why it's different
  from Pi (the WASM sandboxing sentence), install, quickstart link, TUI screenshot,
  link to full docs.

**Exit gate:** a person unfamiliar with the project follows the README install command,
completes the quickstart, and has the model read and edit a file in a real repo.

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
