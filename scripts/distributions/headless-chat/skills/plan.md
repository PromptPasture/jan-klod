---
name: plan
description: Break a task into concrete steps
---

# Breaking Down a Task into Steps

Before diving into code, spend a few minutes planning. A clear plan saves hours of debugging and refactoring.

## Understanding the Task

1. **Read the spec carefully**
   - What is the goal? What should change?
   - What stays the same?
   - Are there constraints (performance, compatibility, security)?

2. **Identify unknowns**
   - What parts of the codebase will you touch?
   - Are there external dependencies or APIs you need to learn?
   - What could go wrong?

3. **Ask clarifying questions** before starting
   - "Do we need backward compatibility?"
   - "What's the scale? (1 user or 1M users?)"
   - "Are there performance targets?"

## Planning the Implementation

### 1. Sketch the Approach

- Write a paragraph describing how you'll solve it at a high level
- Name the files you'll change
- Note any data structures or algorithms you'll use
- List any new dependencies

### 2. Break into Boxes

Each box should be:
- **Checkable:** You can verify it works in isolation or together with other boxes
- **Focused:** Does one thing; doesn't scatter across unrelated concerns
- **Small:** Completable in a single session or short PR review
- **Ordered:** Earlier boxes don't depend on later ones

Example:
```
- [ ] Create database schema and migration
- [ ] Add API endpoint to fetch users
- [ ] Add frontend UI to display users
- [ ] Add tests for the endpoint
- [ ] Performance: add index on frequently-searched columns
```

### 3. Name Files and Modules

- What new files are you creating?
- What files need modification?
- Are there naming conventions to follow?

Example:
```
New:
  - src/features/user_search.rs
  - tests/user_search_tests.rs

Modified:
  - src/lib.rs (add module)
  - docs/api.md (add endpoint docs)
```

### 4. State Assumptions

Write down the assumptions that will guide your work:

- "We'll use PostgreSQL (not SQLite)" → affects schema, connection pooling, migration tool
- "UI will be React" → affects component structure, state management
- "We need <100ms response time" → affects indexing, caching strategy

These let reviewers catch misalignments early.

## Verification

For each box, ask:
- How will I know it works? (test? manual check? benchmark?)
- What could break? (refactor check? lint? type check?)
- What do I need to verify at the end? (all tests pass? performance acceptable?)

## Tips

- **Use a checklist:** Check off boxes as you complete them. Momentum builds.
- **Write before coding:** A written plan catches confusion early.
- **Stay flexible:** Plans change. Document changes so reviewers understand your reasoning.
- **Share your plan:** Get feedback before investing hours. "Does this approach make sense?"
- **Don't over-plan:** For small tasks, a few bullets suffice. For large ones, a doc with structure wins.

## Example Plan

Task: Add email notifications to user sign-up

**Approach:**
Use an existing email service (SendGrid). On sign-up, queue an async job that sends a welcome email. Track delivery status in the database.

**Files:**
- New: `src/email.rs`, `src/jobs/send_email.rs`, migration for `email_logs` table
- Modified: `src/user.rs` (add email_sent field), `src/api.rs` (sign-up endpoint)

**Boxes:**
1. Database: Create schema and migration for user emails and delivery logs
2. Email service: Wrap SendGrid API, handle errors and retries
3. Async job: Add queue system and job handler
4. Integration: Modify sign-up to queue email job
5. Tests: Unit tests for email sending, integration test for full flow

**Assumptions:**
- SendGrid credentials stored in environment
- Email sending failures don't block user creation
- Notifications are non-critical (eventually consistent OK)

**Verification:**
- Unit tests for email module
- Integration test: sign up, check email job was queued
- Manual: create test account, verify email received
