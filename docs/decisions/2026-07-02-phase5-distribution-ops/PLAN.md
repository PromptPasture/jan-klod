# Phase 5 Plan — Distribution & Ops

Living execution checklist for Phase 5 of the [Roadmap](../../concepts/roadmap.md).
Update the flags here and in the roadmap [Status tracker](../../concepts/roadmap.md#status-tracker)
as work proceeds.

**Prerequisite:** Phases 1–4 done (2026-07-02) — the runtime is functionally
complete (thin loop, durable state, REST surface, TUI + Telegram clients,
delegation). What is missing is a way to **ship** it and **keep it updated**.

Relevant background:
[Blue/Green Deployment](../../concepts/blue-green-deployment.md) ·
[Configurator](../../concepts/configurator.md).

## Goal

Ship jan-klod and keep it updatable: a **tiny supervisor/updater** that stages,
flips, health-checks, and rolls back a core+extensions deploy unit without
downtime, and a **Configurator / curated bundles** path so users get a
ready-to-run archive.

## Architecture invariants carried in

- **The supervisor is a separate process from the Rust core**, so it survives a
  core swap (the thing performing the flip cannot be the thing being swapped). It
  is tiny and carries no agent logic.
- **The deploy unit is `core binary + ext/ guests + config.yaml`.** *Updated from
  the concept docs:* Phases 3–4 made the store, the inbound REST surface, Telegram,
  and delegation **host-side (inside the `jan-klod` core binary)** — so `ext/` holds
  the **provider + interceptor (+ future tool) guests**, not `store-sqlite.wasm` /
  `api-rest.wasm`. The core binary is self-contained for persistence and inbound
  network.
- **UI client binaries are separate, optionally-installed artifacts** updated on
  their own; core runs headless without them.

## TBD resolutions (recommended leans — confirmed at the slice that needs each)

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| Supervisor language | Slice 5a | **Go** (static single binary) | Trivial cross-compilation to a dependency-free static binary that survives a core swap; the roadmap already names a "tiny Go launcher/updater". (TinyGo unnecessary — this is a host binary, not a wasm guest.) |
| Health-check mechanism | Slice 5a | A `GET /health` on the core's REST surface (add to `serve`), plus process-liveness | Cheap, already have the HTTP surface; a 200 confirms the swapped core actually booted and can serve. |
| Slot/state model | Slice 5a | `~/.jan-klod/{blue,green}` + an `active` symlink + `state.yaml` (live slot + version history + rollback target) | Matches [blue-green-deployment.md](../../concepts/blue-green-deployment.md); atomic symlink flip gives instant rollback. |
| Configurator delivery (v1) | Slice 5b | **Curated pre-built bundles** first (downloadable archives per preset/os/arch); the interactive Spring-Initializr-style web UI later | A static bundle set is shippable now; the dynamic ZIP-builder needs a backend/WASM build service. GitHub Pages hosts either. |

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Slice | Flag |
|---|---|
| 5a — Supervisor / updater (blue/green stage → flip → health-check → rollback) | `not-started` |
| 5b — Configurator + curated bundles | `not-started` |
| 5c — Exit gate | `not-started` |

---

## Slice 5a — Supervisor / updater

- [ ] **`GET /health` on the core REST surface** — a cheap liveness endpoint the
  supervisor probes after a flip (returns 200 + a small JSON `{status, version}`).
- [ ] **The supervisor (Go)** — manages `~/.jan-klod/{blue,green}`, `active`
  symlink, `state.yaml`. Pure slot/state logic (which slot is live, pick the flip
  target, record rollback target) is unit-tested; process spawn + health probe are
  glue.
- [ ] **Update flow** — stage into the standby slot → validate (checksums; WIT
  interface compatibility of the staged `ext/` against the core) → atomic symlink
  flip → restart → health-check → **PASS** keep / **FAIL** roll back (flip back,
  restart, mark bad).
- [ ] **Supply-chain** — the Go supervisor module passes `go mod verify` +
  `govulncheck` (extend `make go-supply-chain`).

**Exit gate:** a scripted update stages a new slot, flips, health-checks, and (on a
simulated failed health check) rolls back to the previous slot — verified offline.

---

## Slice 5b — Configurator + curated bundles

- [ ] **Curated bundles (v1)** — a small set of ready-to-run archives (per preset ×
  os/arch): the `jan-klod` core, a preset `ext/` set, a pre-filled `config.yaml`,
  and (UI presets) the `jan-klod-ui` binary + an enabled REST surface. A `make
  bundle` produces one locally; publish the matrix from CI on release.
- [ ] **Configurator web UI (later)** — the Spring-Initializr-style selector on
  GitHub Pages that resolves the extension graph and emits a ZIP. Deferred behind the
  static bundles; record the hosting/build-service decision when built.

**Exit gate:** `make bundle` produces a self-contained archive that boots
(`jan-klod` runs from the extracted dir) offline.

---

## Slice 5c — Exit gate

- [ ] Integration test(s): the supervisor's stage→flip→health→rollback cycle (5a)
  and a produced bundle boots (5b), offline where possible.
- [ ] CI — a `make phase5-gate` added to `.github/workflows/ci.yml`.
- [ ] **Mark Phase 5 `done`** here and in [roadmap.md](../../concepts/roadmap.md).
  With Phases 1–5 done, jan-klod is shippable end-to-end.

**Definition of done:** `make phase5-gate` passes in CI; `roadmap.md` status tracker
updated to `done`.

## Cross-cutting (continuous)

- Strict lint on new code (Go supervisor: the repo `golangci-lint` policy); Rust
  changes keep the workspace clippy-clean.
- Supply-chain gates extended to the supervisor module.
- Update the [Blue/Green](../../concepts/blue-green-deployment.md) and
  [Configurator](../../concepts/configurator.md) concept docs to drop the retired
  `store-sqlite.wasm` / `api-rest.wasm` guests from the deploy unit (now host-side).
