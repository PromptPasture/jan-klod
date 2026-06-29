# Decisions

Session handoffs and architecture decision records, newest first.

- [2026-06-29 — Component Model on Rust + Wasmtime](2026-06-29-component-model-rust/Handoff.md) — untrusted extensions ⇒ WASM sandbox; adopt Component Model; host moves Go→Rust (no pure-Go CM host); supersedes JSON-ABI/wazero
- [2026-06-28 — MVP WASM Host](2026-06-28-mvp-wasm-host/Handoff.md) — core-module JSON ABI (wazero has no component model), no-CGo confirmed, SQLite runs host-side not in-guest *(superseded by 2026-06-29)*
- [2026-06-28 — Go + Wazero + WASM Stack](2026-06-28-go-wasm-stack/Handoff.md) — replaced Java/Quarkus with Go + Wazero; locked stack, storage, UI, and deployment decisions
- [2026-06-26 — Small-Model Harness](2026-06-26-small-model-harness/Handoff.md) — design mitigations for reliable agent loops on small LLMs
- [2026-06-16 — Jan-Klod Initial Design](2026-06-16-jan-klod/Handoff.md) — project vision, artifact map, extension taxonomy, open questions (Java/Quarkus era — superseded by 2026-06-28)
