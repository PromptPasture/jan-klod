//! The session store: an append-only event log, the projections derived from
//! it, and the only crate in the tree that knows SQLite (#180).
//!
//! What a session *is* lives here. [`store`] holds rows and never reads their
//! payloads; [`event_log`] owns the versioned envelope those payloads are
//! written in, and the adapters that record a turn as it happens;
//! [`projection`] derives the things a client asks for — a transcript, a
//! resume point, a fork — by replaying the log rather than by keeping a
//! second copy of the answer.
//!
//! # Why it is a crate
//!
//! So that `rusqlite` is a dependency of one crate rather than of everything
//! that can reach a session. `jan-klod-core` linked SQLite into the same
//! binary as the component runtime, and nothing in the type system said which
//! half a given line belonged to. Prior art for the boundary: Pi keeps its
//! SQLite session backend in a package of its own for the same reason.
//!
//! The vocabulary of a turn — `Event`, `Message`, `ToolCall` — comes from
//! [`jan_klod_protocol::turn`], *below* this crate. It cannot come from the
//! loop above, which needs this crate: that is the cycle this arrangement
//! exists to avoid.

pub mod event_log;
pub mod projection;
pub mod store;
