# AGENTS.md

## GitHub Automation

- Keep workflow YAML minimal, readable, and scoped to the repository behavior it automates.
- Prefer existing action/version patterns already used in `.github/workflows` unless there is a clear reason to change.
- Validate workflow syntax after editing GitHub Actions files.
- If workflow behavior changes, update nearby comments or repository docs that describe the automation.

## macOS Minutes Are Billed at 10x

This repository is **private**, so GitHub bills a macOS runner minute at **ten
Linux minutes** against the allowance. That is why `ci-macos.yml` is narrow —
the Seatbelt tests plus the core unit suite, triggered only by the paths that
can break them, plus `workflow_dispatch` — while `ci.yml` runs everything on
every push to `main`.

Widening the macOS job is a **cost decision, not a coverage tweak**. A full
`make gate` there is roughly 15-25 wall minutes, so 150-250 billed minutes per
push, which exhausts a free allowance in a handful of pushes. Before adding
paths, dropping the filter, or promoting it to `make gate`, re-decide it on
[#95](https://github.com/PromptPasture/jan-klod/issues/95), where the trade is
written down.

It is a separate workflow rather than a fourth job in `ci.yml` because `paths`
filters the *workflow* (`on.<push|pull_request>.<paths>`) — there is no per-job
equivalent, so the filter would gate `lint-test`, `harness` and `supply-chain`
as well. `release.yml` also uses `macos-latest` and `macos-13`, so macOS spend
is accepted here for releases; it is per-push spend that is not.
