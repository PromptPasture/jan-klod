# Phase 8 Plan — Tool Fleet

Execution checklist for Phase 8. Update flags here and in [Status tracker](../../concepts/roadmap.md#status-tracker).

> **Amendment (post-8):** three `host-fs` tools consolidated into single **`tool-fs`** component with `op` argument. `tool-fs-probe` retired. `interceptor-permission` gained **op check** (flags mutating ops). Slice notes describe original three-tool design.

**Prerequisite:** Phase 7 done. Substrates exist, proven via probes. Loop can't yet use tools (`AgentSession` wires `NoTools`). No real fleet.

References: [Roadmap Phase 8](../../concepts/roadmap.md#phase-8--tool-fleet) · `ToolExtension` · `ToolInvoker` · `tool-callable.wit`.

## Goal

Make tools usable end-to-end: **wire loop to call `tool-*` extensions** (advertised at `select-tools`, gated at `tool-call`), build first real set over substrates (file read/write/grep, bounded shell).

## Architecture invariants

- **Tools route through substrates, never raw OS** — file tools import `host-fs`, exec tools import `host-process` (default-deny, jailed).
- **Loop has seams** — `select-tools`, `tool-call`, conductor `ToolInvoker`. Phase 8 fills with real extensions.
- **Config-enabled** like extensions (`tool.*`), workspace root + execution enablement (default-deny).

## TBD resolutions

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| Loop ↔ dispatch | 8a | `ToolFleet` holding `ToolExtension`s keyed by name, implements `ToolInvoker` | Reuses tested `ToolExtension` + seam; no conductor change. |
| Config | 8a | Top-level `workspace:` opens `host-fs`; `execution: {enabled, timeout, output-cap}`. Default-deny. | Explicit, opt-in, matches Phase 7 safety. |
| First tools | 8b–8c | `tool-fs-read/-write/-grep` (host-fs); `tool-shell` (host-process) | Highest-value core; rest follow same pattern. |
| Advertise | 8a | `interceptor-tool-selector` exposes fleet metadata on `pending-request.tools` | Seam exists; fill it. |

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Slice | Flag |
|---|---|
| 8a — Wire the loop to tools (`ToolFleet` + config) | `done` |
| 8b — `host-fs` tools (read / write / grep) | `done` |
| 8c — `host-process` tool (shell) | `done` |
| 8d — Exit gate | `done` |

---

## Slice 8a — Wire the loop to tools

- [x] **`ToolFleet`** — implements `ToolInvoker` over `Vec<ToolExtension>`, resolves `meta()` names at construction, dispatches `call.name` → extension's `invoke`. Unknown → `None`. Verified crossing boundary.
- [x] **Config → capabilities** — `Runtime::open_workspace`/`open_process_runner` build **default-deny** capabilities; `build_agent` instantiates each enabled `tool.*` into `ToolFleet`. `AgentSession::run`/`run_streaming_headless` drive with fleet. `tool_wiring.rs` verified.
- [x] **Advertise at `select-tools`** — `build_agent` serves fleet metadata in `host-config`; `interceptor-tool-selector` replaces `pending-request.tools` with advertised `{name, description, schema}`. Tested.

**Done:** tool call reaches matching extension through loop, result feeds back.

---

## Slice 8b — `host-fs` tools

- [x] **`tool-fs-read`/`tool-fs-write`** — guests over `host-fs`. `fs-read({path})`→contents; `fs-write({path, contents})`→confirm. Built + staged.
- [x] **`tool-fs-grep`** — `fs-grep({pattern, path})`→`lineno:line`s. Pure logic tested; reads through `host-fs`. Built + staged.

**Done:** Through `ToolFleet`, `fs-write`→`fs-read`→`fs-grep` operate on workspace file; escapes denied.

---

## Slice 8c — `host-process` tool

- [x] **`tool-shell`** — guest over `host-process`. `shell({command, args?})`→`{code, stdout, stderr}`. Named `shell` for permission gating. Built + staged.

**Done:** Through `ToolFleet` with enabled runner, `shell` runs command, returns output; disabled/escape denied.

---

## Slice 8d — Exit gate

- [x] Integration: `phase8_gate.rs` — config with v1 interceptors + `tool.fs-write` + workspace. Canned model emits `fs-write` call; advertised, gated (permission ask fires, driver approves), fleet dispatches to real `tool-fs-write` writing through `host-fs`, result feeds back, file verified on disk.
- [x] CI — `make phase8-gate` (gate + `tool_fleet` + `tool_wiring`), harness job.
- [x] Mark Phase 8 `done` in [roadmap.md](../../concepts/roadmap.md). Runtime file-workspace-capable. Carried-forward: rest of fleet, symlink/COW, long-lived children.

**Done:** `make phase8-gate` CI green; `roadmap.md` updated. ✓

## Cross-cutting

- Strict clippy; `cargo test` green; guests wired + supply-chain gates.
- Tools behind substrates (default-deny, jailed); dangerous tools gated at `tool-call`.
