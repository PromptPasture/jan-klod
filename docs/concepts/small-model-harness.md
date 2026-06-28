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
6. **Deterministic router** — classify simple intents without LLM; only route complex cases into the agent loop.
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
