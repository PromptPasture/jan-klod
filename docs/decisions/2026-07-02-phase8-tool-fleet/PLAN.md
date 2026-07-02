# Phase 8 Plan — Tool Fleet

Living execution checklist for Phase 8 of the [Roadmap](../../concepts/roadmap.md).
Update the flags here and in the roadmap [Status tracker](../../concepts/roadmap.md#status-tracker)
as work proceeds.

> **Amendment (post-Phase 8):** the three `host-fs` tools shipped here —
> `tool-fs-read`, `tool-fs-write`, `tool-fs-grep` — were later consolidated into a
> single **`tool-fs`** component exposing one tool named `fs`, with the operation
> chosen by an `op` argument (`{op:"read"|"write"|"grep", …}`). The `tool-fs-probe`
> fixture was retired and its host tests (`host_fs.rs`, `tool_fleet.rs`,
> `tool_wiring.rs`) retargeted to `tool-fs`. Because `fs` has a benign name, the
> `interceptor-permission` gate gained an **op check** that flags mutating ops
> (`write`/`delete`/…) — replacing the old "name contains `write`" trigger. The
> slice notes below describe the original three-tool design as built at the time.

**Prerequisite:** Phase 7 done. The `host-fs` and `host-process` substrates exist and
are proven across the CM boundary via `tool_host::ToolExtension` + probe guests — but
the loop can't yet *use* tools (`AgentSession` wires `NoTools`), and there is no real
`tool-*` fleet.

Relevant background: [Roadmap → Phase 8](../../concepts/roadmap.md#phase-8--tool-fleet) ·
`jan_klod_core::tool_host::ToolExtension` · `jan_klod_core::conductor::ToolInvoker` ·
`wit/tool-callable.wit` (the `tool-world` guests implement) · the `tool-fs-probe` /
`tool-proc-probe` pattern from Phase 7.

## Goal

Make tools usable end-to-end and ship a first real set: **wire the loop to call
`tool-*` extensions** (a `ToolInvoker` over instantiated `ToolExtension`s, advertised
at `select-tools`, gated at `tool-call`), then build the highest-value tools over the
substrates — file read/write/grep and a bounded shell.

## Architecture invariants carried in

- **Tools route through the substrates, never the raw OS** — file tools import
  `host-fs`, exec tools import `host-process` (both default-deny, workspace-jailed).
- **The loop already has the seams** — `select-tools` (`interceptor-tool-selector`)
  advertises tools, `tool-call` (`interceptor-permission`) gates them, and the
  conductor dispatches through `ToolInvoker`. Phase 8 fills those seams with real
  extensions; no new loop mechanism.
- **Tools are config-enabled** like any extension (`tool.*`), with the workspace root
  and execution enablement from config (default-deny).

## TBD resolutions (recommended leans — confirmed at the slice that needs each)

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| Loop ↔ tool dispatch | 8a | A `ToolFleet` holding instantiated `ToolExtension`s keyed by `meta().name`, implementing `conductor::ToolInvoker` (match `call.name` → the extension's `invoke`) | Reuses the tested `ToolExtension` + the existing `ToolInvoker` seam; no conductor change. |
| Workspace / exec config | 8a | Top-level `workspace: <root>` opens the `host-fs` `Workspace`; `execution: { enabled, timeout, output-cap }` builds the `host-process` runner. Absent → default-deny. | Explicit, opt-in, matches the Phase 7 safety model. |
| First tool set | 8b–8c | `tool-fs-read`, `tool-fs-write`, `tool-fs-grep` (over `host-fs`); `tool-shell` (over `host-process`) | The highest-value core; edit/ast-*/find/git and ssh/lsp/browser follow the same pattern later. |
| Advertising tools to the model | 8a | `interceptor-tool-selector` exposes the fleet's `meta()` set on `pending-request.tools` | The seam already exists; fill it. |

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

- [x] **`ToolFleet`** — `tool_host::ToolFleet` implements `conductor::ToolInvoker`
  over a `Vec<ToolExtension>`, resolving each tool's advertised name via
  `ToolExtension::meta()` at construction and dispatching `call.name` → the matching
  extension's `invoke`; unknown → `None`. Exposes `tool_names()`/`meta()` for
  advertising. Verified across the boundary (`tool_fleet.rs`: dispatches `fs-probe`
  by name, skips an unknown tool).
- [x] **Config → capabilities** — `Runtime::open_workspace` (top-level `workspace:`)
  and `open_process_runner` (`execution: { enabled, timeout-secs?, output-cap? }`)
  build the shared, **default-deny** `host-fs`/`host-process` capabilities;
  `build_agent` instantiates each enabled `tool.*` as a `ToolExtension` with them into
  a `ToolFleet`, and `AgentSession::run`/`run_streaming_headless` drive the loop with
  the fleet (restructured to avoid the borrow conflict). `tool_wiring.rs`: a config
  enabling `tool.fs-probe` + a workspace yields `tool_names() == ["fs-probe"]`.
- [x] **Advertise at `select-tools`** — `build_agent` serves the fleet's metadata
  as a `tools` array in every interceptor's `host-config` section; the rebuilt
  `interceptor-tool-selector` reads it and **replaces `pending-request.tools`** with
  the advertised `{name, description, parameters-schema}` set (empty → proceed). Tested
  (`tool_selector_advertises_configured_tools`: a `tools` config fills the request).

**Exit gate:** a turn whose model emits a tool call reaches the matching `tool-*`
extension through the loop and feeds the result back (offline, canned provider) —
verified in Slice 8d.

---

## Slice 8b — `host-fs` tools

- [x] **`tool-fs-read` / `tool-fs-write`** — sandboxed `tool-callable` guests over
  `host-fs`. `fs-read({path})` → contents; `fs-write({path, contents})` → a
  confirmation. Built + staged; driven through a `ToolFleet`
  (`fs_write_then_fs_read_through_the_fleet`).
- [x] **`tool-fs-grep`** — `fs-grep({pattern, path})` → matching `lineno:line`s (pure
  match logic unit-tested natively; reads through `host-fs`). Built + staged.

**Exit gate:** ✓ through a `ToolFleet`, `fs-write` → `fs-read` → `fs-grep` operate on a
workspace file; escapes stay denied (Phase 7).

---

## Slice 8c — `host-process` tool

- [x] **`tool-shell`** — a `tool-callable` guest over `host-process`:
  `shell({command, args?})` → `{ code, stdout, stderr }` (JSON). Named `shell` so
  `interceptor-permission` gates it at `tool-call`. Built + staged; driven through a
  `ToolFleet` (`shell_tool_runs_a_command_through_the_fleet`: `echo` → `{code:0,
  stdout}`).

**Exit gate:** ✓ (through a `ToolFleet` with an enabled runner) `shell` runs a command
and returns its output; disabled/escape denied by `host-process` (Phase 7). The
permission-gate-in-the-loop path is exercised in Slice 8d.

---

## Slice 8d — Exit gate

- [x] Integration test: `phase8_gate.rs` — from a config with all v1 interceptors +
  `tool.fs-write` + a workspace, the (canned) model emits an `fs-write` tool call; it
  is advertised at `select-tools`, gated at `tool-call` (the **permission ask** fires
  — `fs-write` contains "write" — and an approving driver allows it via the new
  `AgentSession::run_driven`), the fleet dispatches to the real `tool-fs-write` which
  **writes the file through `host-fs`**, the result feeds back, and the loop returns a
  grounded answer. The file is verified on disk.
- [x] CI — `make phase8-gate` (the gate + `tool_fleet` + `tool_wiring`) wired into the
  harness job.
- [x] **Mark Phase 8 `done`** here and in [roadmap.md](../../concepts/roadmap.md).
  With Phases 1–8 done, the runtime is a file-workspace-capable agent. Carried-forward:
  the rest of the fleet (edit/ast-*/find/git; eval/ssh/lsp/browser; fetch) follows the
  same pattern; symlink/COW + long-lived children (Phase 7 carry-forwards).

**Definition of done:** `make phase8-gate` passes in CI (green); `roadmap.md` status
tracker updated to `done`. ✓

## Cross-cutting (continuous)

- Strict clippy; `cargo test` green per PR; new guests wired into the Makefile +
  supply-chain gates.
- Tools stay behind the substrates (default-deny, workspace-jailed); a dangerous tool
  is gated at `tool-call`.
