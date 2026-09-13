# Phase 7 Plan — File-workspace Substrate

Execution checklist for Phase 7. Update flags here and in [Status tracker](../../concepts/roadmap.md#status-tracker).

**Prerequisite:** Phases 1–6 done. Loop reasons, calls tools, persists, serves, streams. Missing: **filesystem and process access** (sandbox blocks both).

References: [Roadmap Phase 7](../../concepts/roadmap.md#phase-7--file-workspace-substrate) · [architecture.md](../../concepts/architecture.md) · host capabilities pattern.

## Goal

Add **`host-fs`** (scoped workspace read/write) and **`host-process`** (run command). Both host-owned, routed — guest never touches raw OS.

## Safety model (security-sensitive)

Capabilities grant sandboxed guest real reach; mediation is key:

- **Opt-in.** Neither granted unless configured. `host-fs` needs **workspace root** in config; `host-process` needs explicit enable. Default deny.
- **`host-fs` path-jailed.** Every path resolved vs. workspace root, rejected if escapes (`..`, absolute, symlink outside). Guest sees workspace only.
- **`host-process` bounded.** Host user, **cwd jailed to workspace**, **timeout**, **output cap**. Code execution: off by default, gated at `tool-call`.
- **Pure logic unit-tested** (path-jail, truncation); host impls wired + probed.

## TBD resolutions

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| One or two | 7a | **Two** — `host-fs` and `host-process` distinct | Separate WIT; deployment grants independently. |
| `host-fs` v1 ops | 7a | `read`/`write`/`list`/`exists`, workspace-relative | Covers file tools; `ast-*`/checkpoint later. |
| `host-fs` jail | 7a | Resolve vs. root, reject escapes (incl. symlinks). | Simple, auditable; guest sees workspace only. |
| `host-process` v1 | 7b | Run-to-completion `exec()` with timeout + output cap | Common case (bash/eval); streaming children later. |
| Resource limits | 7b | Timeout + output cap v1; heavier OS isolation later | Bounds footguns simply. |

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Slice | Flag |
|---|---|
| 7a — `host-fs` capability | `done` |
| 7b — `host-process` capability | `done` |
| 7c — Exit gate | `done` |

---

## Slice 7a — `host-fs`

- [x] **`wit/host-fs.wit`** — `read`/`write`/`list-dir`/`exists` + `fs-error` + `entry`. `wasm-tools` green.
- [x] **Path-jail (pure, tested)** — `Workspace::resolve` joins + normalizes + prefix-checks vs. root; absolute + `..` escapes denied. 5 tests. Symlink-escape hardening noted as v1 caveat.
- [x] **Host impl** — `ToolExtension` satisfies imports; `host-fs` backed by `Option<Workspace>` — **default-deny**. Added to `tool-world`.
- [x] **Probe guest** — `tool-fs-probe` writes then reads back. Built + staged.

**Done:** `host_fs.rs` — guest round-trips file, escape denied, no-workspace denied, offline.

---

## Slice 7b — `host-process`

- [x] **`wit/host-process.wit`** — `exec(command, args, cwd?, stdin?) -> result<exit>` with `exit{code, stdout, stderr}` + `proc-error`. `wasm-tools` green.
- [x] **Host impl** — `ProcessRunner`: spawns via `std::process::Command`, cwd jailed to workspace, polls with **timeout** (kills on expiry), caps output. **Default-deny**. 7 tests (echo/false/cat + disabled-deny, cwd-escape-deny, timeout, cap). Pipe-deadlock caveat.
- [x] **Wire + probe** — Added to `tool-world`; `tool_host` provides (backed by `ProcessRunner`). `tool-proc-probe` runs command, returns stdout. Built + staged.

**Done:** `host_process.rs` — guest runs command, gets stdout; disabled denied, offline.

---

## Slice 7c — Exit gate

- [x] Tests: `host_fs.rs`, `host_process.rs`, offline.
- [x] CI — `make phase7-gate` (substrate tests + cross-boundary), harness job.
- [x] Mark Phase 7 `done` in [roadmap.md](../../concepts/roadmap.md). Carried-forward: symlink-escape, COW/checkpoint; long-lived children, heavier OS isolation; wiring into `build_agent`.

**Done:** `make phase7-gate` CI green; `roadmap.md` updated. ✓

## Cross-cutting

- Strict clippy; `cargo test` green; new guests wired + supply-chain gates.
- Capabilities stay **default-deny + workspace-jailed**; safety model is contract.
