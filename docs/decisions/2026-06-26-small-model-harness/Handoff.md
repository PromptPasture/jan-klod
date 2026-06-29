---
type: decision
title: Small-Model Harness Design
description: Mitigations for reliable agent loops on small (9–12B) LLMs — constrained decoding, dynamic tool injection, layered router, retry/correction.
tags: [decision, llm, small-model, agent-loop, constrained-decoding]
created: 2026-06-26
updated: 2026-06-26
---

# Handoff Document

**Date:** 2026-06-26
**Topic:** Small-model (9–12B) agent harness design

---

## Context

The user is trying to solve a practical deployment problem: enabling users to run LLM agents **without a large GPU cluster or cloud API access**. The target model size is **9–12B parameters**, running locally.

---

## What Was Discussed

A full architectural breakdown of why small models fail in standard agent loops and how to compensate:

### Root causes of small-model agent failures

- Poor instruction following on long/complex system prompts
- Tool call formatting hallucinations
- Context/state loss over long loops
- Over-planning or stuck reasoning loops

### Key design mitigations (in priority order)

1. **Constrained decoding** — grammar-constrained token generation (llama.cpp `grammar` param, Outlines, vLLM guided decoding) forces valid JSON tool calls. Biggest single win.
2. **Dynamic tool injection** — only expose tools relevant to the current step, not all tools at once.
3. **External state management** — maintain a working memory dict outside the model; compress/summarize history before it bloats context.
4. **Tiny, surgical prompts** — few-shot examples per tool, rewritten per step by a controller layer.
5. **ReAct loop preferred** over Plan-and-Execute for small models.
6. **Deterministic router** — classify simple intents and handle them without LLM; only route genuinely complex cases into the agent loop.
7. **Retry/correction in the harness** — on malformed output, inject a correction hint and retry (up to N times) before failing.

### Recommended models (instruction-tuned)

- Qwen2.5-7B/14B-Instruct — best small-model tool callers currently
- Llama-3.1-8B-Instruct — solid with constrained decoding
- Mistral-Nemo-12B — good instruction following
- Functionary models — fine-tuned for tool use

### Libraries mentioned

|Library|Role|
|---|---|
|Outlines|Grammar-constrained decoding|
|LangGraph|Loop/state management|
|Instructor|Structured outputs via Pydantic|
|llama.cpp|Local inference + grammar param|
|SGLang|Fast local serving + structured gen|

### Proposed architecture

```text
User query
    │
    ▼
Intent router ──→ direct answer (no agent)
    │
    ▼
Step controller (selects tools, compresses history, builds prompt)
    │
    ▼
LLM 9-12B (constrained decoding)
    │
    ▼
Parse & validate action → retry on failure
    │
    ▼
Tool execution → loop back to step controller
    │
    ▼
Answer extractor
```

---

## Open Questions / Next Steps

- **Target deployment not confirmed** — the user was asked about bare metal vs. edge vs. framework preference but did not answer before ending the session. This is the most important thing to clarify first.
- Potential next steps depending on answer:
  - Prototype a minimal harness (Python, specific framework TBD)
  - Benchmark Qwen2.5 vs Llama-3.1 on a tool-calling eval
  - Design the constrained decoding integration layer
  - Design the dynamic tool injection / prompt controller

---

## Suggested Skills

- `/plan` — if the user wants to sequence implementation into milestones
- `/brainstorm` — if exploring harness design options further before committing
- `/write-prd` — if formalizing requirements for the harness product
- `/write-ticket` — if breaking work into implementation tickets

---

## Notes for Next Agent

Start by asking: **"What's your target deployment environment?"** (bare metal Linux, edge device, specific framework like LangGraph/llama.cpp, etc.) — the answer will determine which tradeoffs and library choices to prioritize.
