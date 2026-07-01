# Thin Loop + Interceptor Middleware (2026-07-01)

Re-architecting the agent loop after reviewing Pi's design: keep jan-klod's
sandbox + small-model foundation, adopt Pi's thin loop as **core mechanism**, and
express every agent decision as a **sandboxed interceptor extension**.

- [Brainstorm](BRAINSTORM.md) — the four forks (hook mechanism, loop location,
  harness placement, v1 scope), the decisions, and the open questions (interceptor
  WIT shape, the UI-prompt gap, chain ordering, fallback expressibility).

Feeds the re-architected [Phase 2 plan](../2026-07-01-phase2-agent-loop/PLAN.md).
