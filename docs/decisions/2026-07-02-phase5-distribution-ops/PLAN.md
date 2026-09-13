# Phase 5 Plan — Distribution & Ops

Execution checklist for Phase 5. Update flags here and in [Status tracker](../../concepts/roadmap.md#status-tracker).

**Prerequisite:** Phases 1–4 done (2026-07-02) — runtime functionally complete. Missing: **ship** it + **keep updated**.

References: [Blue/Green Deployment](../../concepts/blue-green-deployment.md) · [Configurator](../../concepts/configurator.md).

## Goal

Ship jan-klod with updates: **tiny supervisor/updater** (stage, flip, health-check, rollback) + **Configurator/curated bundles** for ready-to-run archives.

## Architecture invariants

- **Supervisor separate from core** — survives core swap. Tiny, no agent logic.
- **Deploy unit: `core binary + ext/ guests + config.yaml`.** Phases 3–4 made store, REST, Telegram, delegation host-side (in core binary). `ext/` holds **provider + interceptor guests** only. Core self-contained.
- **UI client binaries separate**, optionally installed, updated independently. Core headless without them.

## TBD resolutions

| TBD | First needed | Recommended lean | Why |
|---|---|---|---|
| Supervisor language | Slice 5a | **Go** (static binary) | Cross-compiles to dependency-free binary, survives core swap. Roadmap names it. |
| Health-check | Slice 5a | `GET /health` on REST surface (+ process-liveness). | Cheap; confirms swapped core booted + serves. |
| Slot/state | Slice 5a | `~/.jan-klod/{blue,green}` + `active` symlink + `state.yaml` | Matches [blue-green.md](../../concepts/blue-green-deployment.md); symlink flip instant rollback. |
| Configurator v1 | Slice 5b | **Curated pre-built bundles** (downloadable per preset/os/arch). Web UI later. | Static bundles shippable now; ZIP-builder needs backend service. |

## Status

Flags: `not-started` · `in-progress` · `blocked` · `done`.

| Slice | Flag |
|---|---|
| 5a — Supervisor / updater (blue/green stage → flip → health-check → rollback) | `done` |
| 5b — Configurator + curated bundles | `done` |
| 5c — Exit gate | `done` |

---

## Slice 5a — Supervisor / updater

- [x] **`GET /health` on core REST** — `serve::health()` returns `200 {status:"ok", version}`; `serve_once` routes `GET /health` (else `POST /turn`). Unit-tested + socket roundtrip in `api_rest.rs`.
- [x] **Supervisor (Go, stdlib-only static binary)** — pure model unit-tested: `Slot`/`Standby`/`Plan`/`Update.Resolve`/`State` (JSON), `AfterHealth`; `Activate`/`ActiveSlot` (atomic symlink flip); `Probe`/`ProbeWithRetries` (HTTP `/health`); `Promote` — full flip→start→health→commit/rollback cycle behind injected seams. `main` wires `status`+`promote`. 8 tests; `go vet`+`golangci-lint` clean.
- [x] **Update flow** — `Promote`: healthy commits standby active; unhealthy stops bad core, flips symlink back. *(Staging — download + checksum/WIT-compat validation — layered later.)*
- [x] **Supply-chain** — `go mod verify`+`govulncheck` on supervisor.

**Done:** `Promote` tests verify flip→health→commit/rollback both ways.

---

## Slice 5b — Configurator + curated bundles

- [x] **Curated bundle** — `scripts/bundle.sh` assembles ready-to-run archive: release `jan-klod` core, staged guests (`ext/`), pre-filled `config.yaml`, README, tarred as `dist/jan-klod-<version>-<os>-<arch>.tar.gz`. Verified: extracted `./jan-klod` boots offline, loads bundled guests. *(Per-preset × os/arch matrix + UI-client inclusion layered later.)*
- [ ] **Configurator web UI (later)** — Spring-Initializr-style selector on GitHub Pages, emits ZIP. Deferred; record hosting decision.

**Done:** `make bundle` produces self-contained archive; extracted `jan-klod` boots offline.

---

## Slice 5c — Exit gate

- [x] Tests: supervisor flip→health→rollback (5a); `make bundle` archive boots offline (5b).
- [x] CI — `make phase5-gate` (supervisor `go test`) in `lint-test` job.
- [x] Mark Phase 5 `done` in [roadmap.md](../../concepts/roadmap.md). Jan-klod shippable end-to-end. Carried-forward: staging, bundle matrix, web Configurator.

**Done:** `make phase5-gate` CI green; `roadmap.md` updated. ✓

## Cross-cutting

- Strict lint (Go supervisor: `golangci-lint`); Rust clippy-clean.
- Supply-chain gates to supervisor.
- [x] Updated [Blue/Green](../../concepts/blue-green-deployment.md) + [Configurator](../../concepts/configurator.md) docs: `ext/` holds guests only; core binary has persistence/REST/Telegram/delegation.
