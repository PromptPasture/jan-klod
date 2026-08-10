# Concepts

- [Roadmap](roadmap.md) — phased build plan from the Rust + Wasmtime foundation to a shippable runtime
- [Architecture](architecture.md) — overall system design, extension model, agent loop
- [Security model](security-model.md) — what a component is granted, how, where it is enforced, and which test proves it; plus the gaps
- [Contracts](contracts.md) — stable WIT interfaces between core and extensions
- [Configuration](configuration.md) — the `config.yaml` format and how the core loads it into extension instances
- [Small-Model Harness](small-model-harness.md) — design principles for 9–12B LLM agent loops
- [Blue/Green Deployment](blue-green-deployment.md) — zero-downtime update and rollback strategy
- [Configurator](configurator.md) — web UI for generating deployment archives
