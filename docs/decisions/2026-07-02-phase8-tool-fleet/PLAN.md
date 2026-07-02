# Phase 8 Plan — Tool Fleet

Living execution checklist for Phase 8 of the [Roadmap](../../concepts/roadmap.md).
Update the flags here and in the roadmap [Status tracker](../../concepts/roadmap.md#status-tracker)
as work proceeds.

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
| 8a — Wire the loop to tools (`ToolFleet` + config) | `in-progress` |
| 8b — `host-fs` tools (read / write / grep) | `not-started` |
| 8c — `host-process` tool (shell) | `not-started` |
| 8d — Exit gate | `not-started` |

---

## Slice 8a — Wire the loop to tools

- [x] **`ToolFleet`** — `tool_host::ToolFleet` implements `conductor::ToolInvoker`
  over a `Vec<ToolExtension>`, resolving each tool's advertised name via
  `ToolExtension::meta()` at construction and dispatching `call.name` → the matching
  extension's `invoke`; unknown → `None`. Exposes `tool_names()`/`meta()` for
  advertising. Verified across the boundary (`tool_fleet.rs`: dispatches `fs-probe`
  by name, skips an unknown tool).
- [ ] **Config → capabilities** — `Runtime` reads a workspace root + execution
  settings; `build_agent` instantiates each enabled `tool.*` as a `ToolExtension`
  (with the shared `host-fs` workspace / `host-process` runner) into a `ToolFleet`,
  and `AgentSession::run*` drives the loop with it instead of `NoTools`.
- [ ] **Advertise at `select-tools`** — `interceptor-tool-selector` fills
  `pending-request.tools` from the fleet's metadata (via `host-config`).

**Exit gate:** a turn whose model emits a tool call reaches the matching `tool-*`
extension through the loop and feeds the result back (offline, canned provider).

---

## Slice 8b — `host-fs` tools

- [ ] **`tool-fs-read` / `tool-fs-write` / `tool-fs-grep`** — sandboxed
  `tool-callable` guests over `host-fs`. `read({path})`, `write({path, contents})`,
  `grep({pattern, path?})` (line matches). Built + staged; each `invoke` returns
  JSON.

**Exit gate:** through the loop (or `ToolExtension` directly), read/write/grep operate
on a workspace file; escapes stay denied.

---

## Slice 8c — `host-process` tool

- [ ] **`tool-shell`** — a `tool-callable` guest over `host-process`:
  `shell({command, args?})` → `{ code, stdout, stderr }`. Gated at `tool-call` by
  `interceptor-permission` (it is dangerous by name).

**Exit gate:** the loop runs a shell command via the tool, the permission gate can
deny it, and the result feeds back.

---

## Slice 8d — Exit gate

- [ ] Integration test: a turn drives at least one `host-fs` tool (read/write) and the
  `host-process` `tool-shell` through the loop, with the permission gate exercised —
  offline.
- [ ] CI — `make phase8-gate` added to `.github/workflows/ci.yml`.
- [ ] **Mark Phase 8 `done`** here and in [roadmap.md](../../concepts/roadmap.md).
  With Phases 1–8 done, the runtime is a file-workspace-capable agent.

**Definition of done:** `make phase8-gate` passes in CI; `roadmap.md` status tracker
updated to `done`.

## Cross-cutting (continuous)

- Strict clippy; `cargo test` green per PR; new guests wired into the Makefile +
  supply-chain gates.
- Tools stay behind the substrates (default-deny, workspace-jailed); a dangerous tool
  is gated at `tool-call`.
