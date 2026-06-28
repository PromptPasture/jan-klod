---
type: concept
title: Contracts
description: Stable Java interfaces that form the boundary between core and extensions
tags: [contracts, interfaces, extensions, api]
created: 2026-06-28T00:00:00Z
updated: 2026-06-28T00:00:00Z
---

Contracts are the stable interfaces that the core exposes and extensions consume or implement. They must be versioned carefully — a breaking change here breaks all extensions.

## Core contracts

| Interface | Responsibility |
|---|---|
| `LlmProvider` | Sends prompts to a language model; returns completions (optionally constrained) |
| `ContextManager` | Manages conversation history; compresses/summarises as context grows |
| `AgentManager` | Drives the agent loop (intent routing → step controller → LLM → tool → answer) |
| `MemoryStore` | Persistent key-value or vector store for long-term agent memory |
| `SkillRegistry` | Registers and resolves reusable agent skills |
| `McpRegistry` | Manages MCP server connections and tool discovery |

## Design rules

- Core only depends on contracts, never on extension implementations.
- Extensions declare which contracts they require and which they provide.
- Optional dependencies must degrade gracefully (feature off, not crash).

## Status

Contracts are **not yet designed**. The natural next step is defining Java interface signatures precisely enough to scaffold the Maven multi-module project. See the open question in [decisions/2026-06-16-jan-klod/Handoff.md](../decisions/2026-06-16-jan-klod/Handoff.md).
