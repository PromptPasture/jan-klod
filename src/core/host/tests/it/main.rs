//! The host's integration suite, as **one** test target.
//!
//! These files used to each be their own `tests/` binary. `wasmtime` links
//! statically as a dev-dependency, so that was ~4GB of duplicated binaries and
//! a wasmtime link step per file on every `cargo test`. One target, one link;
//! restricting *which* tests run doesn't help, since cargo still compiles and
//! links every target regardless of what you select.
//!
//! Consequence: these modules now share a process, and a few set
//! process-global env vars (answer timeouts, credential leak-canaries) that
//! race under `cargo test`'s in-binary thread parallelism. Run it with `cargo
//! nextest run`, which gives one process per test — as `make test`, `make
//! harness` and `make gate` all now do.

mod common;

mod agent_loop;
mod api_prompt;
mod api_rest;
mod auth;
mod classifier;
mod component_harness;
mod docs_match_config;
mod egress_boundary;
mod gate;
mod host_fs;
mod host_process;
mod installed_layout;
mod local_model;
mod persistence;
mod polyglot;
mod prompt_disconnect;
mod provider_chain;
mod sandbox_boundary;
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
