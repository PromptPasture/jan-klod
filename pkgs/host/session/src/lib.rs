//! The session store: an append-only event log and its projections; the only crate that knows SQLite.

pub mod event_log;
pub mod projection;
pub mod store;
