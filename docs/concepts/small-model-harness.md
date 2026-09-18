---
type: concept
title: Small-Model Harness
description: Design principles for running reliable agentic loops on 9-12B parameter models
tags: [llm, small-model, constrained-decoding, agent-loop]
created: 2026-06-28T00:00:00Z
updated: 2026-07-01T00:00:00Z
---

Small instruction-tuned models (9–12B parameters) fail in agent loops predictably. The harness mitigates them systematically.

## Root causes

- Context overflow — long histories corrupt attention
- Malformed tool calls — model outputs invalid JSON
- Tool overload — too many tools in one prompt
- Prompt sensitivity — minor wording changes cause regressions

## Where each mitigation lives

Since 2026-07-01 the loop is a thin **core** conductor and every *decision* is a sandboxed **interceptor** extension — see [Thin Loop + Interceptor Middleware](../decisions/2026-07-01-thin-loop-interceptors/BRAINSTORM.md). Mitigations split cleanly:

- **Fixed core mechanism** (always on): constrained-decoding **grammar passthrough** to the provider, action **parse + validation**, and **retry-with-correction** on malformed output.
- **Interceptor extensions** (independently enabled): layered router, dynamic tool injection, external state/compression, and per-request prompt shaping. Frontier-model deployments can drop these and keep the bare loop.

## Mitigations (priority order)

1. **Constrained decoding** *(core)* — grammar-constrained generation (llama.cpp, Outlines, vLLM) forces valid JSON. Biggest win. The loop passes a `grammar` on `completion-request`; the provider executes it. Grammar *construction* is a core default, overridable at request-shaping (derives from active tool set).
2. **Dynamic tool injection** *(→ `interceptor-tool-selector`, `select-tools`)* — expose only tools relevant to the current step.
3. **External state management** *(→ `interceptor-context`, `select-context`)* — maintain working memory outside the model; compress history before context bloats.
4. **Surgical prompts** *(→ request-shaping)* — few-shot examples per tool.
5. **ReAct loop** preferred over Plan-and-Execute for small models — this *is* the core loop's shape.
6. **Layered router** *(→ `interceptor-intent-router`, `before-loop`)* — classify intents in two tiers before the agent loop:
   - **Language detection** (µs) — pure-Rust `whatlang` (no model). Non-English → tier 2.
   - **Tier 1: heuristics** (English, µs) — ~50 grouped rules (greetings, farewells, affirmations, meta-queries, clarifications, short inputs). Catches obvious intents at zero cost. Grouped, not flat — add one rule per line.
   - **Tier 2: LLM** — one constrained call to `llm-provider`, output one token: `simple` | `agentic`. All languages, no separate embedding model; reuses the loaded provider.
7. **Retry/correction** *(core)* — on malformed output, inject a hint and retry (up to N times). Retry is loop-iteration control in core; policy (on/off, N, hint) from `host-config`.

## External state management (mitigation 3) — half built

`tool-plan` exists: a model-facing tool holding a list of steps, one mutation
per call, the whole plan returned every time. `scope: session` keeps two
sessions apart, enforced host-side so the guest holds no session id and
cannot get the isolation wrong ([#215]).

That covers the "working memory outside the model" half of mitigation 3. The
"compress history before context bloats" half is still
`interceptor-context`'s and is unchanged.

**Whether it helps is not yet known, and should not be assumed.** The survey
it came from ([#194]) found that Codex, Claude Code and oh-my-pi all ship a
plan tool, and that Pi refuses one outright — "they confuse models" — while
shipping four tools in total. Our models are smaller than any of theirs,
which cuts both ways: more need for external working memory, less capacity
to use a tool well. Nothing here has been measured against a real 9–12B model
on real tasks.

What a measurement would need: a task long enough to exceed the context a
small model tracks reliably, run with the tool enabled and disabled, counting
re-derivations of intent rather than task success. **A negative result is a
complete outcome** and belongs in this section — the tool is one line of
config to disable and one directory to delete.

[#194]: https://github.com/PromptPasture/jan-klod/issues/194
[#215]: https://github.com/PromptPasture/jan-klod/issues/215

## Edit reliability (`tool-edit`) — built

Small models produce shaky edits: hallucinated line numbers, re-emitted files, or anchors to moved text. The sandboxed [`tool-edit`](../../src/extensions/tool-edit/src/lib.rs) guest mitigates this with a **hash-anchored patch format** — constrained decoding applied to edits.

Two properties make it reliable:

1. **Content-hash anchoring.** Every line gets an **anchor** — a short hash of *number and text* (`fnv1a32(lineno \0 text)`), handed to the model by `op=view` as `anchor|lineno|text`. Edits name anchors, never raw line numbers. Before applying, the tool re-derives anchors from the live file: if the line moved or changed, the edit is **rejected with nothing written**. Stale edits fail loudly instead of corrupting code. Hash collisions fail the same way (ambiguous → rejected), costing a retry, never a bad write.
2. **A grammar for the format.** Patch syntax is a small JSON op set (`view`, `replace` over an anchor span, `insert` before/after an anchor; delete is `replace` with empty contents), so the model is **constrained to emit only valid patches** via the loop's `grammar` field — the biggest small-model win carried into editing. *(Wiring the schema into grammar is a follow-up; the op set is already narrow.)*

Rejections return as **ok results explaining recovery** ("run op=view again and retry with fresh anchors"), not as `tool-error` — which carries no message. `err` is reserved for malformed arguments and `host-fs` failures.

The patcher works on file *content*, needing **no raw filesystem access**: every read/write goes through `host-fs`, confining the tool to the workspace like any other guest.

## Recommended models

Instruction-tuned models suitable for this harness:

- Qwen2.5 (7B / 14B)
- Llama-3.1 (8B)
- Mistral-7B-Instruct

## Open questions

- Target deployment: bare metal vs. edge vs. cloud.
- Benchmark Qwen2.5 vs Llama-3.1 on tool-calling eval before picking a default.

See [decisions/2026-06-26-small-model-harness/Handoff.md](../decisions/2026-06-26-small-model-harness/Handoff.md) for the full session record.
