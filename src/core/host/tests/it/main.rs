//! The host's integration suite, as **one** test target.
//!
//! These twenty-nine files used to sit directly in `tests/`, which made each of
//! them its own test binary. `wasmtime` is a dev-dependency of this crate and
//! links statically, so that was twenty-nine binaries of 5MB–191MB — 4.0GB, and
//! twenty-nine separate wasmtime link steps on every `cargo test` that touched
//! the host workspace. As one target it is one binary and one link.
//!
//! Worth stating because it is the thing people reach for first and it does not
//! work: restricting *which* tests run — `#[ignore]`, a name filter, an
//! integration-only CI job — changes none of this. Cargo compiles and links
//! every target under `tests/` whatever you select to run. Only the number of
//! targets moves the cost.
//!
//! The consequence to keep in mind when adding tests here: these modules now
//! share a process. They did not before, and several of them set process-global
//! environment variables — `JK_ANSWER_TIMEOUT_SECS` at two different values,
//! and two distinct pairs of credential leak-canaries. Under `cargo test`'s
//! in-binary thread parallelism those race. `cargo nextest run` (`make
//! test-fast`) gives one process per test, which is the isolation the old
//! file-per-binary layout provided for free. A test that depends on
//! process-global state belongs behind nextest, not behind a `cargo test` run.

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
