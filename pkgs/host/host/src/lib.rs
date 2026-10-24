//! Outbound transports over a kernel `AgentSession`, public so tests can drive them in-process.

pub mod acp;
pub mod config_write;
pub mod mcp;
pub mod pending;
pub mod rpc;
pub mod serve;
pub mod session_thread;
pub mod sessions;
pub mod setup;
pub mod telegram;
mod ws;
