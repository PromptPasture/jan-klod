---
name: review
description: Review code for correctness and clarity
---

# Code Review Checklist

A thorough code review catches bugs early and spreads knowledge across the team. Use this checklist as your guide:

## Correctness

- [ ] Does the code do what it claims to do?
- [ ] Are all edge cases handled (null, empty, boundary conditions)?
- [ ] Is error handling appropriate? Are errors logged or returned clearly?
- [ ] Are there potential race conditions or deadlocks?
- [ ] Do loops terminate correctly?
- [ ] Are array/buffer bounds checked?

## Logic & Algorithms

- [ ] Is the algorithm correct? Can you trace through a few examples?
- [ ] Is there a simpler way to solve this problem?
- [ ] Are temporary variables and naming clear?
- [ ] Could this be a library function instead?

## Testing

- [ ] Are there tests for the new code?
- [ ] Do tests cover happy path, edge cases, and errors?
- [ ] Are test names descriptive?
- [ ] Are there enough tests? (not just checking one assertion)

## Performance

- [ ] Could this code create a memory leak?
- [ ] Are there unnecessary allocations or copies?
- [ ] Could a database query N+1 problem occur?
- [ ] Is the Big-O complexity acceptable?

## Style & Maintainability

- [ ] Does the code follow the project's conventions?
- [ ] Are variable and function names clear and consistent?
- [ ] Are comments needed? (good code is self-documenting)
- [ ] Is the code DRY (Don't Repeat Yourself)?
- [ ] Could this be broken into smaller functions?

## Security

- [ ] Is user input validated and sanitized?
- [ ] Are secrets handled correctly (not logged, not in VCS)?
- [ ] Is there proper authorization/authentication?
- [ ] Are dependencies up to date?

## Documentation

- [ ] Are function signatures documented?
- [ ] Are non-obvious behaviors explained?
- [ ] Are there doc comments for public APIs?

## Review Best Practices

- **Be constructive:** "Consider using a HashMap here for O(1) lookup" is better than "This is slow."
- **Ask questions:** "What happens if X is null?" helps the author think through edge cases.
- **Praise good code:** "Nice use of the builder pattern" reinforces good habits.
- **Focus on code, not person:** Review what was written, not who wrote it.
- **Approve small changes quickly:** Don't let perfect be the enemy of good.

## Common Red Flags

- Code that is hard to understand on first read
- Commented-out code (delete it, use git history if needed)
- Magic numbers without explanation
- Exception handling that swallows errors
- Copy-pasted code in multiple places
