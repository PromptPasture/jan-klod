//! One test target instead of separate binaries. Previously each was ~4GB
//! (wasmtime static linking) with redundant link steps. Single target, single link.
//! Even restricting test selection doesn't help—cargo links all targets.
//!
//! Modules share a process but set process-global env vars (timeouts,
//! leak-canaries) that race under `cargo test`'s thread parallelism.
//! Use `cargo nextest run` for per-test isolation (as `make test`, `make harness`,
//! `make gate` do).

mod common;

mod acp;
mod agent_loop;
mod api_prompt;
mod api_rest;
mod auth;
mod bundle_distributions;
mod classifier;
mod client_surface;
mod component_harness;
mod docs_match_config;
mod egress_boundary;
mod event_log;
mod execution_config;
mod ext_install;
mod ext_registry;
mod gate;
mod generated_guest;
mod grep_budget;
mod guardrails;
mod host_fs;
mod host_process;
mod installed_layout;
mod lazy_tool_fleet;
mod local_model;
mod manifest;
mod mcp;
mod persistence;
mod polyglot;
mod prompt_disconnect;
mod provider_chain;
mod registry_stdio;
mod rpc;
mod sandbox_boundary;
mod sandbox_landlock;
mod sandbox_seatbelt;
mod session_memory;
mod shipped_defaults;
mod storage_scope;
mod telegram;
mod test_layout;
mod tool_edit;
mod tool_fetch;
mod tool_find;
mod tool_fleet;
mod tool_git;
mod tool_wiring;
mod wasm_cache;
mod web_client;
