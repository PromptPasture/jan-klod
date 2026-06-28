---
type: concept
title: Small-Model Harness
description: Design principles for running reliable agentic loops on 9-12B parameter models
tags: [llm, small-model, constrained-decoding, agent-loop]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T00:00:00Z
---

Small instruction-tuned models (9–12B parameters) fail in agent loops for predictable reasons. The harness mitigates these systematically.

## Root causes of failure

- Context overflow — long histories corrupt attention
- Malformed tool calls — model outputs invalid JSON
- Tool overload — too many tools in one prompt
- Prompt sensitivity — minor wording changes cause regressions

## Mitigations (priority order)

1. **Constrained decoding** — grammar-constrained token generation (llama.cpp `grammar`, Outlines, vLLM guided decoding) forces valid JSON tool calls. Biggest single win.
2. **Dynamic tool injection** — only expose tools relevant to the current step.
3. **External state management** — maintain a working memory dict outside the model; compress/summarise history before it bloats context.
4. **Tiny, surgical prompts** — few-shot examples per tool, rewritten per step by the controller.
5. **ReAct loop** preferred over Plan-and-Execute for small models.
6. **Layered router** — classify intents in two tiers before entering the agent loop:
   - **Language detection** (microseconds) — pure-Go library (`whatlanggo` or similar, no model). If non-English → skip to tier 2 directly.
   - **Tier 1: heuristics** (English only, microseconds) — up to ~50 rules grouped by category (greetings, farewells, affirmations, meta-queries, clarifications, short inputs). Catches obvious simple intents at zero model cost. Rules are grouped, not a flat pile — adding one rule means one line in the right category.
   - **Tier 2: LLM classifier** — everything that passes through goes to the active `llm-provider` with a single constrained-decoding call; output is one token: `simple` | `agentic`. Handles all languages naturally. No separate embedding model; reuses the already-loaded provider.
7. **Retry/correction** — on malformed output, inject a correction hint and retry (up to N times) before failing.

## Recommended models

Instruction-tuned models suitable for this harness:

- Qwen2.5 (7B / 14B)
- Llama-3.1 (8B)
- Mistral-7B-Instruct

## Open questions

- Target deployment not confirmed: bare metal vs. edge vs. cloud framework.
- Benchmark Qwen2.5 vs Llama-3.1 on a tool-calling eval before picking a default.

See [decisions/2026-06-26-small-model-harness/Handoff.md](../decisions/2026-06-26-small-model-harness/Handoff.md) for the full session record.
