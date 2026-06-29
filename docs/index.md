# Wiki

- [concepts/](concepts/) — core concepts: architecture, design principles, extension model
- [decisions/](decisions/) — architecture decisions and session handoffs
- [guides/](guides/) — setup and how-to instructions for building from source

## Overview

Jan-Klod is a self-hosted, extensible AI agent runtime. The **core** is a minimal Rust + Wasmtime host (a native binary); all domain logic lives in **extensions** — sandboxed WebAssembly components defined by WIT contracts and authored in any language. See [Architecture](concepts/architecture.md) for the full picture and [decisions/2026-06-29-component-model-rust](decisions/2026-06-29-component-model-rust/Handoff.md) for the foundation decision.
