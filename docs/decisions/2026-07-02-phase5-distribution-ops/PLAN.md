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
| 5a — Supervisor / updater (blue/green stage → flip → health-check → rollback) | `done` |
| 5b — Configurator + curated bundles | `done` |
| 5c — Exit gate | `done` |

---

## Slice 5a — Supervisor / updater

- [x] **`GET /health` on the core REST surface** — `serve::health()` returns
  `200 {status:"ok", version}`; `serve_once` routes `GET /health` (else `POST /turn`).
  Unit-tested + exercised over a real socket in `api_rest.rs` (a second round-trip).
- [x] **The supervisor (Go, `src/supervisor/`)** — a stdlib-only static binary.
  Pure model unit-tested: `Slot`/`Standby`/`Plan`/`Update.Resolve`/`State`
  (JSON, not YAML, to stay dependency-free) `AfterHealth`; `Activate`/`ActiveSlot`
  (atomic symlink flip via temp+rename); `Probe`/`ProbeWithRetries` (HTTP `/health`);
  and `Promote` — the full flip → start → health → commit/rollback cycle behind
  injected `start`/`probe` seams. `main` wires `status` + `promote` (spawn core +
  probe). 8 tests; `go vet` + `golangci-lint` clean.
- [x] **Update flow (flip → health → commit/rollback)** — implemented + tested by
  `Promote`: healthy commits the standby slot active; unhealthy stops the bad core
  and flips the `active` symlink back. *(Staging the standby slot — download +
  checksum/WIT-compat validation — is the release/download side, layered on later;
  the flip/rollback mechanism it feeds is done.)*
- [x] **Supply-chain** — `make supply-chain` runs `go mod verify` + `govulncheck` on
  the supervisor (stdlib-only; `golangci-lint` clean).

**Exit gate:** ✓ `Promote` unit tests verify the offline cycle both ways — a healthy
promotion flips to and commits the standby slot; a simulated failed health check
stops the bad core and rolls the `active` symlink back to the previous slot.

---

## Slice 5b — Configurator + curated bundles

- [x] **Curated bundle (`make bundle`)** — `scripts/bundle.sh` assembles a
  ready-to-run archive: the release `jan-klod` core, the staged provider/interceptor/
  tool guests (`ext/`), a pre-filled `config.yaml`, and a README, tarred as
  `dist/jan-klod-<version>-<os>-<arch>.tar.gz`. Verified: the extracted `./jan-klod`
  boots offline, loading its guests from the bundled `ext/`. *(The per-preset ×
  os/arch matrix + UI-client inclusion is a CI-release concern layered on the same
  script.)*
- [ ] **Configurator web UI (later)** — the Spring-Initializr-style selector on
  GitHub Pages that resolves the extension graph and emits a ZIP. Deferred behind the
  static bundles; record the hosting/build-service decision when built.

**Exit gate:** ✓ `make bundle` produces a self-contained archive; the extracted
`jan-klod` boots offline (verified — loads bundled guests + runs lifecycle).

---

## Slice 5c — Exit gate

- [x] Tests: the supervisor's flip→health→rollback cycle is unit-tested both ways
  (5a); `make bundle` produces an archive whose extracted `jan-klod` boots offline
  (5b, verified).
- [x] CI — `make phase5-gate` (supervisor `go test`) runs in the `lint-test` job
  (`.github/workflows/ci.yml`); a `setup-go` step backs `make test` + the gate.
- [x] **Mark Phase 5 `done`** here and in [roadmap.md](../../concepts/roadmap.md).
  With Phases 1–5 done, jan-klod is shippable end-to-end. Carried-forward,
  non-blocking: staging (download + checksum/WIT-compat validation), the per-preset ×
  os/arch bundle matrix, and the interactive web Configurator.

**Definition of done:** `make phase5-gate` passes in CI (green); `roadmap.md` status
tracker updated to `done`. ✓

## Cross-cutting (continuous)

- Strict lint on new code (Go supervisor: the repo `golangci-lint` policy); Rust
  changes keep the workspace clippy-clean.
- Supply-chain gates extended to the supervisor module.
- [x] Updated the [Blue/Green](../../concepts/blue-green-deployment.md) and
  [Configurator](../../concepts/configurator.md) concept docs: the deploy unit's
  `ext/` holds provider/interceptor/tool guests only — persistence, the REST surface,
  telegram, and delegation are host-side in the core binary (Phase 3/4).
