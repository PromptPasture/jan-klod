//! The outbound surfaces the `jan-klod-gateway` binary serves: transports over a kernel `AgentSession` (#179).
//! The modules are public so integration tests can drive a surface without a subprocess.

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
