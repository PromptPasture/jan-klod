# Phase 7 Plan — File-workspace Substrate

Living execution checklist for Phase 7 of the [Roadmap](../../concepts/roadmap.md).
Update the flags here and in the roadmap [Status tracker](../../concepts/roadmap.md#status-tracker)
as work proceeds.

**Prerequisite:** Phases 1–6 done. The loop can reason, call tools, persist, serve,
and stream — but the sandbox grants **no filesystem and no process access**, so no
file- or execution-shaped tool can exist yet.

Relevant background: [Roadmap → Phase 7](../../concepts/roadmap.md#phase-7--file-workspace-substrate) ·
[architecture.md → Extension model](../../concepts/architecture.md) ·
existing host capabilities (`host-http`, `host-storage`) as the pattern to mirror.

## Goal

Add the two host-mediated capabilities everything file/execution-shaped depends on:
**`host-fs`** (a scoped workspace read/write view) and **`host-process`** (run a
command). Both are host-owned and routed — the guest never touches the raw OS —
mirroring how `host-http` inverts outbound network.

## Safety model (this is the security-sensitive phase)

These capabilities hand a sandboxed guest real reach, so the mediation is the point:

- **Opt-in.** Neither is granted unless configured. `host-fs` needs a **workspace
  root** in config; `host-process` needs to be explicitly enabled. Default deny.
- **`host-fs` is path-jailed.** Every path is resolved against the workspace root and
  rejected if it escapes it (`..`, absolute paths, symlink targets outside the root).
  The guest sees a workspace, never the wider filesystem.
- **`host-process` is bounded.** Runs under the host user (no extra OS sandbox in v1
  — a documented limitation), with a **cwd jailed to the workspace**, a **timeout**,
  and an **output cap**. It is code execution: it is off by default and the loop
  gates it at `tool-call` (`interceptor-permission`).
- **Pure logic is unit-tested** (path-jail resolution, output truncation); the host
  impls are wired into the linker and exercised by a probe guest.

## TBD resolutions (recommended leans — confirmed at the slice that needs each)

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| One capability or two | 7a | **Two** — `host-fs` and `host-process` are distinct concerns (the roadmap calls them co-equal) | Separate WIT interfaces; a deployment can grant one without the other. |
| `host-fs` v1 ops | 7a | `read` / `write` / `list` / `exists` (+ `delete`?), all workspace-relative | Covers the file tools (read/write/edit/find/grep); `ast-*`/`checkpoint`/COW are later. |
| `host-fs` jail | 7a | Resolve against the workspace root, reject any path that escapes it (incl. symlink targets); root from config | Simple, auditable; the guest can only see its workspace. |
| `host-process` v1 model | 7b | **Run-to-completion** `exec(command, args, cwd?, stdin?) -> {exit-code, stdout, stderr}` with a timeout + output cap | The common case (bash/eval); a long-lived/streaming child (ssh, lsp, browser) is deferred. |
| Resource limits | 7b | Timeout + max captured output (v1); heavier isolation (rlimits, namespaces, seccomp) is OS-specific and deferred | Bounds the obvious footguns without OS-specific complexity yet. |

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Slice | Flag |
|---|---|
| 7a — `host-fs` capability | `done` |
| 7b — `host-process` capability | `done` |
| 7c — Exit gate | `done` |

---

## Slice 7a — `host-fs`

- [x] **`wit/host-fs.wit`** — host-provided `read`/`write`/`list-dir`/`exists`
  (renamed from `list` — reserved keyword) + `fs-error` (`not-found`/`denied`/`io`)
  + `entry`. `wasm-tools component wit wit/` green.
- [x] **Path-jail (pure, unit-tested)** — `host_fs::Workspace::resolve` joins +
  lexically normalizes (`.`/`..`) and prefix-checks against the canonicalized root;
  absolute + `..` escapes denied. `Workspace` also does `read`/`write`/`list_dir`/
  `exists`. 5 tests (roundtrip, not-found, escapes denied, inner `..` allowed, sorted
  list). Symlink-escape hardening noted as a v1 caveat.
- [x] **Host impl in core** — `tool_host::ToolExtension` instantiates a `tool-world`
  guest and satisfies its imports (`host-log`/`host-config`/`host-http` + `host-fs`);
  `host-fs` is backed by an `Option<Workspace>` — **default-deny** when `None`.
  `host-fs` added to `tool-world` (the world Phase 8 file tools use).
- [x] **Probe guest** — `tool-fs-probe` (a `tool-callable` guest importing `host-fs`):
  `invoke({path, contents})` writes then reads back. Built + staged.

**Exit gate:** ✓ `host/tests/host_fs.rs` — the guest round-trips a workspace file
through `host-fs`, a `..` escape is denied, and a call with **no workspace** is denied
(default-deny) — all offline. Wired into `make harness`.

---

## Slice 7b — `host-process`

- [x] **`wit/host-process.wit`** — `exec(command, args, cwd?, stdin?) -> result<exit,
  proc-error>` with `exit { code, stdout, stderr }` and `proc-error`
  (`denied`/`timeout`/`spawn-failed`). `wasm-tools` green.
- [x] **Host impl** — `host_process::ProcessRunner`: spawns via `std::process::Command`,
  cwd jailed to the workspace (reusing `Workspace::resolve`), polls `try_wait` to a
  **timeout** (kills on expiry), and caps captured output. **Default-deny** via
  `ProcessRunner::disabled`. 7 host-side tests with real commands (echo/false/cat +
  disabled-deny, cwd-escape-deny, timeout via `sleep`, output cap). Pipe-deadlock on
  huge output noted as a v1 caveat.
- [x] **Wire + probe guest** — `host-process` added to `tool-world`; `tool_host`
  provides it (backed by `ProcessRunner`) and grew a `process` param. `tool-proc-probe`
  (a `tool-callable` guest importing `host-process`) runs a command and returns stdout.
  Built + staged.

**Exit gate:** ✓ `host/tests/host_process.rs` — the guest runs a command through
`host-process` and receives its stdout; a call with execution **disabled** is denied
(default-deny) — offline. Wired into `make harness`.

---

## Slice 7c — Exit gate

- [x] Integration tests: `host_fs.rs` (guest write→read, escape + no-workspace denied)
  and `host_process.rs` (guest run→capture, disabled denied), offline.
- [x] CI — `make phase7-gate` (substrate unit tests + the two cross-boundary tests)
  wired into the harness job.
- [x] **Mark Phase 7 `done`** here and in [roadmap.md](../../concepts/roadmap.md).
  Carried-forward, non-blocking: symlink-escape hardening + COW/checkpoint for `host-fs`;
  long-lived/streaming children + heavier OS isolation (rlimits/namespaces) for
  `host-process`; wiring the substrates into `build_agent` (the loop's tools).

**Definition of done:** `make phase7-gate` passes in CI (green); `roadmap.md` status
tracker updated to `done`. ✓

## Cross-cutting (continuous)

- Strict clippy on new code; `cargo test` green per PR; new guests wired into the
  Makefile + supply-chain gates.
- The capabilities stay **default-deny + workspace-jailed**; the safety model above
  is the contract, and any relaxation is a recorded decision.
