# AGENTS.md

## Session Start

Before any work, read `docs/index.md` for the current wiki state.

- Wiki location: `docs/`
- For wiki operations use the `wiki` skill.
- For substantial work needing durable documentation, create `docs/decisions/YYYY-MM-DD-task-name/`.

## Repository Behavior

- Update project-scoped documents in the same change if behavior they describe is affected.

## Comments

- Doc comments (`///`, `//!`): one line, only what the name, signature and body do not say. Required sections (`# Errors`, `# Panics`) take one line.
- Put the reason in the commit message or an issue, not in a comment.

## Track Work in GitHub Issues

For bugs, future problems, or features: open GitHub issue (not chat/local files). Issues preserve history and progress.

```console
gh issue list --state all --search "keyword sort:created-asc"   # look for an existing issue first, oldest first
gh issue create --title "..." --body-file body.md --label "bug,P2"
```

- Check open and closed issues first. If missing, open before fixing — problem record is as valuable as the fix.
- Exception: fix uncommitted defects without an issue (it's part of the task, not history).
- Title states what is wrong or what should exist. Body gives the steps to reproduce, or what "done" looks like, plus the files involved.
- Use `--body-file` (inline double quotes execute backticks, deleting paths/identifiers).
- Label from `.github/workflows/labels.yml` (source of truth; only use declared labels).
- One type (`bug`, `feature`, `refactor`, `chore`, `docs`, `security`) + one priority (`P0`–`P3`). Add `needs-triage`, `needs-repro`, `blocked`, `declined`, `stale`, `help-wanted` as needed.
- All issues (open/closed) carry both labels, so queries by type/priority return full history.
- Comment findings and attempts to spare repeat investigation.
- Reference the issue from the commit or pull request that fixes it (`Fixes #12`), so it closes together with the change.

## Artifact Quality

- Every artifact must be complete, actionable, internally consistent, and specific enough to verify.
- No placeholders, TODOs, unsupported claims, or missing required sections unless the user requests a draft.
- Every section and example must contribute to the outcome. Remove anything that does not.
- Examples must be narrow, direct, complete, and consistent with actual repo interfaces.
- Self-review every non-trivial artifact for placeholders, contradictions, scope drift, and missing verification. Fix issues before presenting.
