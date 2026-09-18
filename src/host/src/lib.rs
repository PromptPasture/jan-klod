//! The outbound surfaces the `jan-klod-gateway` binary serves.
//!
//! A surface is a transport: it accepts a request in some wire idiom, drives a
//! kernel [`AgentSession`](jan_klod_core::AgentSession), and writes the result
//! back in that idiom. None of it is agent behaviour, which is why it lives
//! here rather than in the kernel — `jan-klod-core` loads extensions, runs the
//! loop and owns the session; this crate only exposes it (#179).
//!
//! The binary is the only production caller. The modules are public because the
//! integration tests drive a surface directly, without a subprocess.

pub mod acp;
pub mod mcp;
pub mod pending;
pub mod rpc;
pub mod serve;
pub mod session_thread;
pub mod telegram;
// Private: the socket is reached through `serve`'s router, never named by
// a caller. `pub` items inside stay crate-visible.
mod ws;
