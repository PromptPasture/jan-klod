---
name: commit
description: Write a clear commit message
---

# Writing Clear Commit Messages

A good commit message helps future developers (including yourself) understand why a change was made, not just what was changed. Use these principles:

## Structure

1. **Subject line** (50 characters or fewer)
   - Summarize the change concisely
   - Use imperative mood: "add feature" not "added feature"
   - Don't end with a period

2. **Body** (when needed)
   - Explain the "why" and "what", not the "how"
   - Wrap at 72 characters
   - Separate from subject with a blank line
   - Use bullet points or paragraphs to explain context

3. **Footer** (when needed)
   - Reference issues: `Fixes #123`, `Refs #456`
   - Note breaking changes: `BREAKING CHANGE: description`

## Examples

**Good:**
```
Fix race condition in session initialization

Previously, the session could be accessed before the lock was acquired,
causing concurrent modification errors. Now we ensure the lock is held
for the entire initialization sequence.

Fixes #789
```

**Poor:**
```
fixed a bug
```

## Conventional Commits (Recommended)

Structure commits as `<type>(<scope>): <subject>`:
- **feat:** a new feature
- **fix:** a bug fix
- **refactor:** code change without behavior change
- **test:** adding or updating tests
- **docs:** documentation changes
- **chore:** build, dependencies, tooling
- **perf:** performance improvement

Example: `feat(auth): add OAuth2 support`

## Tips

- Each commit should be a logical unit of work
- Commit frequently—small, focused changes are easier to review and revert
- Run tests before committing
- Avoid committing unrelated changes together
