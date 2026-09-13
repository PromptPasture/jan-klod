# Wiki

- [concepts/](concepts/) — architecture, design principles, extension model
- [decisions/](decisions/) — architecture decisions and session handoffs
- [guides/](guides/) — setup and build instructions

## Overview

Jan-Klod is a self-hosted, extensible AI agent runtime. The **core** is a minimal Rust + Wasmtime host; all domain logic lives in **extensions** — WIT-defined WebAssembly components in any language. See [Architecture](concepts/architecture.md) for the full picture and [decisions/2026-06-29-component-model-rust](decisions/2026-06-29-component-model-rust/Handoff.md) for the foundation decision. Post-v0.1.0 vision — versioned client protocol, event-sourced session log, OS-level effect sandbox, signed registry, and web/GUI clients with MCP/ACP ports — is in [Vision — Harness as a Platform](decisions/2026-09-08-harness-platform-vision/Vision.md), phased in the [Roadmap](concepts/roadmap.md) as Phases 13–18.
