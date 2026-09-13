# AGENTS.md

## GitHub Automation

- Keep workflow YAML minimal, readable, and scoped to the repository behavior it automates.
- Prefer existing patterns unless clear reason to change.
- Validate workflow syntax after editing GitHub Actions files.
- If behavior changes, update comments and docs.

## macOS Minutes Are Billed at 10x

This repository is **private**, so macOS runner minutes bill at **10x** Linux minutes. That is why `ci-macos.yml` is narrow: Seatbelt tests, boot path (`execution_config::` per [#125](https://github.com/PromptPasture/jan-klod/issues/125)), core unit suite (only on relevant paths), plus `workflow_dispatch`. `ci.yml` runs full suite on every `main` push.

Widening macOS jobs is a **cost decision**, not coverage. Full `make gate`: ~15-25 wall min = 150-250 billed min per push (exhausts free allowance quickly). Before adding paths, dropping filters, or promoting to `make gate`, re-decide per [#95](https://github.com/PromptPasture/jan-klod/issues/95).

**Two halves cost differently (#125 measured)**: Adding a *path* is expensive (wakes whole job, ~3.5 wall min = ~35 billed). Adding a *filter* module is cheap (`execution_config::` adds 5.5s = ~1 billed min). Path without matching filter pays job cost for untouched tests. Widen both or neither.

Separate workflow (not a `ci.yml` job) because `paths` filters workflow-level, not per-job — filter would gate `lint-test`, `harness`, `supply-chain` too. `release.yml` accepts macOS cost for releases; per-push spend is not.
