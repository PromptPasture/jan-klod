//! `jan-klod-host` — the host container built on the neutral [`jan_klod_core`]
//! runtime.
//!
//! [`jan_klod_core`] deliberately holds *zero* agent behaviour: it loads
//! extensions and drives their lifecycle, nothing more. This crate adds the
//! first behaviour on top — the [`agent`] loop, the host-side seed of the
//! eventual `manager.agent-loop` extension. It drives one turn end-to-end over
//! the Component Model (provider completion → memory-store persistence), which
//! is exactly Phase 1's exit gate.
//!
//! The `jan-klod` binary uses [`agent::run_turn`] with the live blocking HTTP
//! client; the offline exit-gate test drives the same entry point with a canned
//! HTTP backend, so the whole loop runs with no network and no API key.

pub mod agent;
