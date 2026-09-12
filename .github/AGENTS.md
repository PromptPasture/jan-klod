# AGENTS.md

## GitHub Automation

- Keep workflow YAML minimal, readable, and scoped to the repository behavior it automates.
- Prefer existing action/version patterns already used in `.github/workflows` unless there is a clear reason to change.
- Validate workflow syntax after editing GitHub Actions files.
- If workflow behavior changes, update nearby comments or repository docs that describe the automation.

## macOS Minutes Are Billed at 10x

This repository is **private**, so GitHub bills a macOS runner minute at **ten
Linux minutes** against the allowance. That is why `ci-macos.yml` is narrow —
the Seatbelt tests, the boot path that resolves them (`execution_config::`, per
[#125](https://github.com/PromptPasture/jan-klod/issues/125)) and the core unit
suite, triggered only by the paths that can break them, plus
`workflow_dispatch` — while `ci.yml` runs everything on every push to `main`.

Widening the macOS job is a **cost decision, not a coverage tweak**. A full
`make gate` there is roughly 15-25 wall minutes, so 150-250 billed minutes per
push, which exhausts a free allowance in a handful of pushes. Before adding
paths, dropping the filter, or promoting it to `make gate`, re-decide it on
[#95](https://github.com/PromptPasture/jan-klod/issues/95), where the trade is
written down.

**The two halves cost very differently, which #125 measured.** Adding a *path*
is the expensive one: every extra wake is a whole job, ~3.5 wall minutes and so
~35 billed. Adding a module to the *filter* is not — `execution_config::`'s 17
tests cost 5.5 s on top of the Seatbelt four, about one billed minute. So a
path added without the filter to match pays almost the whole cost to wake a job
that will not run the tests the edit touched. Widen both, or neither.

It is a separate workflow rather than a fourth job in `ci.yml` because `paths`
filters the *workflow* (`on.<push|pull_request>.<paths>`) — there is no per-job
equivalent, so the filter would gate `lint-test`, `harness` and `supply-chain`
as well. `release.yml` also uses `macos-latest` and `macos-13`, so macOS spend
is accepted here for releases; it is per-push spend that is not.
