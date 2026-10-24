//! The session store: an append-only event log and the projections derived from it.
//! It is the only crate that knows SQLite, so `rusqlite` stays out of the component runtime.
//! Turn vocabulary (`Event`, `Message`, `ToolCall`) comes from `jk_protocol::turn`, below this crate.

pub mod event_log;
pub mod projection;
pub mod store;
