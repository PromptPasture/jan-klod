# AGENTS.md

## Session Start

Before any work, read `docs/index.md` for the current wiki state.

- Wiki location: `docs/`
- For wiki operations use the `wiki` skill.
- For substantial work needing durable documentation, create `docs/decisions/YYYY-MM-DD-task-name/`.

## Repository Behavior

- Repo is source of truth. Verify memory and prior notes against it before acting.
- Limit changes to the minimum required. Do not refactor, reformat, or improve unrelated code without explicit approval.
- Update project-scoped documents in the same change if behavior they describe is affected.
- Final response must state: what changed, what verification ran, and any residual risk.

## Before Editing (Non-Trivial Changes)

A change is non-trivial when it affects behavior, multiple files, shared interfaces, structure, dependencies, generated artifacts, or project docs.

Before editing, state: requested outcome and scope, working assumptions, simplest viable approach, verification plan, and any material ambiguity. If an ambiguity could materially change scope, ask one concise question first.

## Track Work in GitHub Issues

When you find a bug, notice a problem worth fixing later, or plan a new feature, open a GitHub issue for it. Do not leave it in chat or in a local notes file: the issue is what keeps the history of the problem and the progress on it.

```console
gh issue list --state all --search "keyword"   # look for an existing issue first
gh issue create --title "..." --body-file body.md --label "bug,P2"
```

- Before you fix something, check that an issue for it exists, in the open ones and the closed ones alike. If none does, open it first, then fix it: the record of what was wrong is worth as much as the fix.
- The exception is a defect that exists only in changes you have not committed yet. Correcting your own work in progress is part of the task at hand, not project history, so fix it and file nothing.
- Title states what is wrong or what should exist. Body gives the steps to reproduce, or what "done" looks like, plus the files involved.
- Write the body to a file and pass `--body-file`. Inline in double quotes, the shell runs every backtick in it as a command, which silently deletes the path or the identifier you were quoting.
- Label from what `.github/workflows/labels.yml` declares. That workflow is the source of truth, since it creates and updates every label this repository is meant to have, so a label it does not declare is not one to reach for.
- Give every issue one type and one priority. Type is `bug`, `feature`, `refactor`, `chore`, `docs` or `security`. Priority runs `P0` for critical through `P3` for low, and an issue without one is an issue nobody can order against the rest. Add any of `needs-triage`, `needs-repro`, `blocked`, `declined`, `stale` and `help-wanted` that apply.
- Every issue in the tracker carries both, including the closed ones, so a query by type or by priority returns the whole history rather than the part that happened to be labelled that way.
- Record progress on the issue as you go, commenting on what you found and what you tried, so the next person does not repeat the investigation.
- Reference the issue from the commit or pull request that fixes it (`Fixes #12`), so it closes together with the change.

## Artifact Quality

- Every artifact must be complete, actionable, internally consistent, and specific enough to verify.
- No placeholders, TODOs, unsupported claims, or missing required sections unless the user requests a draft.
- Every section, example, and abstraction must contribute to the outcome. Remove anything that does not.
- Examples must be narrow, direct, complete, and consistent with actual repo interfaces.
- Introduce abstractions only when they reduce real complexity or follow an established pattern.
- No speculative features, unused extension points, or unrequested configurability.
- KISS: prefer the simplest complete solution. If an implementation grows beyond what the problem requires, simplify before finalizing.
- Self-review every non-trivial artifact for placeholders, contradictions, scope drift, and missing verification. Fix issues before presenting.
