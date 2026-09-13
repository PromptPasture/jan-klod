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

Enable agents on **9–12B models locally** without large GPU clusters or cloud API.

---

## Discussion

Why small models fail + compensations:

### Failures
- Poor instruction following (complex prompts)
- Tool call hallucinations
- Context/state loss
- Over-planning / stuck loops

### Mitigations (priority order)

1. **Constrained decoding** — grammar forces valid JSON tool calls (Outlines, vLLM). Biggest win.
2. **Dynamic tool injection** — expose only current-step tools.
3. **External state** — working memory dict; compress history before bloat.
4. **Surgical prompts** — few-shot per tool, rewritten per step.
5. **ReAct** over Plan-Execute.
6. **Deterministic router** — handle simple intents without LLM.
7. **Retry/correction** — hint + retry on malformed output (up to N times).

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

## Next Steps

**Clarify target deployment first** (bare metal/edge/framework). Then:
- Prototype minimal harness
- Benchmark Qwen2.5 vs Llama-3.1
- Design constrained decoding integration
- Design tool injection / prompt controller

---

## Skills

- `/plan` — sequence milestones
- `/brainstorm` — explore design further
- `/write-prd` — formalize requirements
- `/write-ticket` — break into work items

---

## Notes for Next Agent

Start by asking: **"What's your target deployment environment?"** (bare metal Linux, edge device, specific framework like LangGraph/llama.cpp, etc.) — the answer will determine which tradeoffs and library choices to prioritize.
