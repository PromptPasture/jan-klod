---
title: Two turns at once — per-session agents, not a resumable turn
description: Phase 20's gate needs two sessions live at the same time. Measured: a second AgentSession costs 1.35 MB and 3.5 ms, and Runtime is already Send + Sync. The alternative needs the conductor to suspend a turn at two known call sites.
---

# Two turns at once

Phase 20 ([#169](https://github.com/PromptPasture/jan-klod/issues/169)) closes on a gate its slices do not reach:

> Two clients drive turns in two sessions **concurrently** over REST, with an `ask` answered in one while the other streams.

#223 put the session behind a job queue, #225 stopped a parked confirmation from holding the socket, and #224 and #226 changed the transport. None of them changes the shape underneath: there is one `AgentSession` on one thread, `run_turn` holds it for the length of a turn, and a turn parked on an `ask` holds it while a human thinks. Two turns serialise. ([#228](https://github.com/PromptPasture/jan-klod/issues/228))

## What the gate asks for, exactly

The clause after the comma is narrower than the word before it. *"An `ask` answered in one while the other streams"* is satisfied by a turn that **yields while parked** — nothing needs to compute at the same time. *"Concurrently"* on its own suggests two models generating at once, which is a strictly stronger thing.

**This record takes the stronger reading.** Not because the example demands it, but because the weaker one is an accident of which example was written down: a user with two sessions open expects both to work, and "the second one waits until the first finishes thinking" is the behaviour the phase set out to remove. A gate satisfied only in the parked case would leave the obvious complaint unaddressed.

## The two routes

**Per-session ownership.** One `AgentSession` per session id, each on its own thread, each reached by the job queue that already exists.

**A turn that yields while parked.** `Driver::ask` becomes a suspension point: the turn returns a partial state, the answer arrives, the turn resumes. This is the shape `wit/interceptor.wit` and `wit/tool-askable.wit` chose for *guests*, with the reasoning written down — "no guest is ever on the stack while the loop is suspended".

## Three measurements

**A second `AgentSession` costs 1.35 MB and 3.5 ms.** Measured against a booted fleet (provider, selector, `tool-fs`, tool instances forced): five extra agents took 6.8 MB of RSS between them, and each took ~3.5 ms after the first (8.4 ms, warm-up included). The store is **already shared** — `AgentSession` holds `Arc<Mutex<store::Store>>` — so two sessions write one database and duplicate only their wasm.

**`Runtime` is `Send + Sync`.** A compile probe passes, so a per-session thread can build its *own* agent rather than being handed one across a boundary an `!Send` value cannot cross. `build_agent` takes `&self` and is already called twice in one test, so this is composition rather than a new capability.

**A turn blocks on a person in exactly two places.** `intercept.rs:404` (an interceptor's `Decision::Ask` — the permission gate) and `tool_host.rs:499` (a tool's `invoke-asking`, #216). Both are `driver.ask(&prompt)` inside the turn. The guest has already yielded at both; it is the host's own call that blocks.

## The decision

**Per-session ownership.** At 1.35 MB a session, a hundred live sessions cost 135 MB — and a deployment with a hundred *simultaneously live* sessions is not the product this is. The alternative asks the conductor to become resumable, which is a change to the one component the architecture describes as "mechanism only" and which every phase so far has kept small.

It also delivers the stronger property. Yielding-while-parked would satisfy the gate's example and still leave two thinking turns serialised.

## What this changes, stated before it surprises anyone

**Interceptor state becomes per-session.** `interceptor-permission` keeps standing grants ("always allow `fs:write`") run-scoped today; per-session agents make them per-session. That is arguably what a user means by "always" — this conversation — but it *is* a change, and anything that counts across a deployment (a rate limit, a budget) would silently count per session instead. No shipped interceptor does that today; one that wants to will need the distinction.

**Tool state becomes per-session**, and here it is plainly right: `tool-plan`'s working memory, and `tool-proc`'s long-lived children (#220), belong to the conversation that started them. A session's dev server is not another session's.

**Adoption needs coordination.** `Runtime::adopt_installed` takes `&mut self`, and per-session agents are built from a shared `Runtime`. Installing an extension mid-session (#213, #214) has to rebuild the agents that should see it. Today one agent is rebuilt at a turn boundary; with several, "which sessions see the new tool, and when" becomes a question with an answer that must be written down rather than implied.

**Memory is now proportional to live sessions.** Nothing evicts an idle session's agent. At 1.35 MB this is not urgent, and a cap is easier to add once something needs one than to design against now.

## Next slices

1. **A session owns an agent.** A registry of session id → (thread, job queue), each thread building its own agent from the shared `Runtime`. The surface routes a request to the session's queue instead of the one queue.
2. **The gate, as a test.** Two sessions, two clients: one parked on an `ask` while the other streams to completion, then the first answered and finishing. Today that test hangs until the answer timeout and then passes for the wrong reason, which is worth writing down before writing it.
3. **Adoption across sessions**, and the sentence in the docs that says which sessions see a newly installed tool.
4. **What per-session state means**, in `docs/concepts/` — one paragraph for the permission gate's grants, since that is the one a user can notice.

The rejected route is not closed off. If memory per session ever becomes the constraint, the two suspension points are named above and the shape to copy is already in two WIT contracts.
