---
type: concept
title: Small-Model Harness
description: Design principles for running reliable agentic loops on 9-12B parameter models
tags: [llm, small-model, constrained-decoding, agent-loop]
created: 2026-06-28T00:00:00Z
updated: 2026-07-01T00:00:00Z
---

Small instruction-tuned models (9–12B parameters) fail in agent loops for predictable reasons. The harness mitigates these systematically.

## Root causes of failure

- Context overflow — long histories corrupt attention
- Malformed tool calls — model outputs invalid JSON
- Tool overload — too many tools in one prompt
- Prompt sensitivity — minor wording changes cause regressions

## Where each mitigation lives

Since 2026-07-01 the loop is a thin **core** conductor and every *decision* is a
sandboxed **interceptor** extension — see
[Thin Loop + Interceptor Middleware](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md).
The mitigations below split cleanly:

- **Fixed core mechanism** (always on, not removable by config): constrained-decoding
  **grammar passthrough** to the provider, action **parse + structural validation**,
  and **retry-with-correction** on malformed output.
- **Interceptor extensions** (independently enabled, tuned per deployment): the
  layered router, dynamic tool injection, external state / compression, and
  per-request prompt shaping. On a frontier-model deployment you can drop these and
  keep the bare loop.

## Mitigations (priority order)

1. **Constrained decoding** *(core mechanism)* — grammar-constrained token generation (llama.cpp `grammar`, Outlines, vLLM guided decoding) forces valid JSON tool calls. Biggest single win. The loop always passes a `grammar` on the `completion-request`; the provider executes it. Grammar *construction* is a core default, overridable at a request-shaping phase (it derives from the active tool set, so typically after `select-tools`).
2. **Dynamic tool injection** *(→ `interceptor-tool-selector`, `select-tools` phase)* — only expose tools relevant to the current step.
3. **External state management** *(→ `interceptor-context`, `select-context` phase)* — maintain working memory outside the model; compress/summarise history before it bloats context.
4. **Tiny, surgical prompts** *(→ a request-shaping phase)* — few-shot examples per tool, applied by a request-shaping interceptor.
5. **ReAct loop** preferred over Plan-and-Execute for small models — this *is* the shape of the core loop.
6. **Layered router** *(→ `interceptor-intent-router`, `before-loop` hook)* — classify intents in two tiers before entering the agent loop:
   - **Language detection** (microseconds) — pure-Rust library (`whatlang`, no model). If non-English → skip to tier 2 directly.
   - **Tier 1: heuristics** (English only, microseconds) — up to ~50 rules grouped by category (greetings, farewells, affirmations, meta-queries, clarifications, short inputs). Catches obvious simple intents at zero model cost. Rules are grouped, not a flat pile — adding one rule means one line in the right category.
   - **Tier 2: LLM classifier** — everything that passes through goes to the active `llm-provider` with a single constrained-decoding call; output is one token: `simple` | `agentic`. Handles all languages naturally. No separate embedding model; reuses the already-loaded provider.
7. **Retry/correction** *(core mechanism)* — on malformed output, inject a correction hint and retry (up to N times) before failing. Retry is loop-iteration control, so it lives in the core loop; the policy (on/off, N, hint template) is read from `host-config`.

## Edit reliability (`tool-edit`) — built

Small models produce shaky file edits: they hallucinate line numbers, re-emit whole
files, or anchor a change to text that has since moved. The sandboxed
[`tool-edit`](../../src/extensions/tool-edit/src/lib.rs) guest mitigates this with a
**hash-anchored patch format** — the same idea as constrained decoding, applied to
edits.

Two properties make it reliable:

1. **Content-hash anchoring.** Every line is addressed by an **anchor** — a short
   hash of that line's *number and text* (`fnv1a32(lineno \0 text)`), handed to the
   model by `op=view` as `anchor|lineno|text`. An edit names anchors, never raw line
   numbers. Before applying, the tool re-derives the anchors from the live file: if
   the line moved or changed, no anchor resolves and the edit is **rejected with
   nothing written**. Stale edits fail loudly instead of corrupting code. A hash
   collision fails the same way (ambiguous → rejected), so it costs a retry, never a
   bad write.
2. **A grammar for the format.** The patch syntax is a small JSON op set (`view`,
   `replace` over an anchor span, `insert` before/after an anchor; delete is
   `replace` with empty contents), so the model can be **constrained to emit only
   valid patches** via the loop's `grammar` field — the biggest small-model win (see
   *Constrained decoding* above) carried into editing. *(Wiring the schema into the
   grammar field is a follow-up; the op set is already narrow enough to validate.)*

Rejections come back as an **ok result whose text says how to recover** ("run
op=view again and retry with fresh anchors"), not as a `tool-error` — `tool-error`
carries no message, and the model needs the reason to self-correct. `err` is reserved
for malformed arguments and `host-fs` failures.

Because the patcher works on file *content*, it needs **no raw filesystem access**:
every read and write goes through the path-jailed `host-fs` substrate, so the tool is
confined to the workspace like any other guest.

## Recommended models

Instruction-tuned models suitable for this harness:

- Qwen2.5 (7B / 14B)
- Llama-3.1 (8B)
- Mistral-7B-Instruct

## Open questions

- Target deployment not confirmed: bare metal vs. edge vs. cloud framework.
- Benchmark Qwen2.5 vs Llama-3.1 on a tool-calling eval before picking a default.

See [decisions/2026-06-26-small-model-harness/Handoff.md](../decisions/2026-06-26-small-model-harness/Handoff.md) for the full session record.
