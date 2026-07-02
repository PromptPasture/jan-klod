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
| 7a — `host-fs` capability | `not-started` |
| 7b — `host-process` capability | `not-started` |
| 7c — Exit gate | `not-started` |

---

## Slice 7a — `host-fs`

- [ ] **Design `wit/host-fs.wit`** — a host-provided interface: `read(path) ->
  result<string, fs-error>`, `write(path, contents) -> result<_, fs-error>`,
  `list(path) -> result<list<entry>, fs-error>`, `exists(path) -> bool`; an
  `fs-error` enum (`not-found`, `denied`, `io`). Add to `wit/README.md`; validate
  with `wasm-tools component wit wit/`.
- [ ] **Path-jail (pure, unit-tested)** — `resolve(root, requested) -> Result<PathBuf,
  Denied>` that joins + canonicalizes and rejects any escape of `root`.
- [ ] **Host impl in core** — a `host-fs` backed by a configured workspace root;
  `add_to_linker` so an interceptor/tool guest importing `host-fs` gets it. No
  workspace configured → all ops `denied`.
- [ ] **Probe guest** — a minimal guest importing `host-fs` that writes then reads a
  file back, to exercise the capability across the CM boundary offline.

**Exit gate:** a sandboxed guest writes and reads a workspace file through `host-fs`,
and a `..`/absolute path escape is denied — verified offline.

---

## Slice 7b — `host-process`

- [ ] **Design `wit/host-process.wit`** — `exec(command, args: list<string>, cwd:
  option<string>, stdin: option<string>) -> result<exit, proc-error>` where `exit =
  { code: s32, stdout: string, stderr: string }`; `proc-error` (`denied`, `timeout`,
  `spawn-failed`). Validate.
- [ ] **Host impl** — spawn via `std::process::Command`, cwd jailed to the workspace,
  a timeout, and an output cap (truncate). Disabled → `denied`.
- [ ] **Probe guest** — a guest that runs a trivial command (`echo`) and reads back
  stdout.

**Exit gate:** a sandboxed guest runs a command through `host-process` and receives
its stdout/exit code; a disabled/over-timeout/escaping call is denied — offline.

---

## Slice 7c — Exit gate

- [ ] Integration test(s): a guest exercises `host-fs` (write→read, escape denied)
  and `host-process` (run→capture, denied when off), offline.
- [ ] CI — `make phase7-gate` added to `.github/workflows/ci.yml`.
- [ ] **Mark Phase 7 `done`** here and in [roadmap.md](../../concepts/roadmap.md);
  begin Phase 8 (the `tool-*` fleet over these substrates).

**Definition of done:** `make phase7-gate` passes in CI; `roadmap.md` status tracker
updated to `done`.

## Cross-cutting (continuous)

- Strict clippy on new code; `cargo test` green per PR; new guests wired into the
  Makefile + supply-chain gates.
- The capabilities stay **default-deny + workspace-jailed**; the safety model above
  is the contract, and any relaxation is a recorded decision.
