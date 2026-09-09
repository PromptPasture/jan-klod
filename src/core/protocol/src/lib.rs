//! The client protocol: one versioned wire contract shared by every client.
//!
//! Today a client is whatever `jan_klod_core::serve` happens to serve — REST
//! routes plus an SSE stream — so each new client (TUI, web, editor) reads the
//! route table and drifts from the others. This crate makes the surface a
//! contract of the same rank as the WIT package: named commands, named
//! notifications, and a version that says when either changed.
//!
//! Two things are deliberately *not* here:
//!
//! * **No transport.** No sockets, no framing, no request ids. A [`Command`]
//!   carries the `method`/`params` pair and nothing else, so the stdio and
//!   WebSocket transports can each wrap it in their own envelope.
//! * **No policy.** These are the shapes of what clients say and what the core
//!   reports back; whether a given command is *allowed* is the core's and its
//!   interceptors' business.
//!
//! REST + SSE do not go away — they become one projection of this contract.
//!
//! # Versioning
//!
//! [`PROTOCOL_VERSION`] is semver. Removing a command, renaming one, or
//! removing a field from one bumps **major**; adding a command, or adding an
//! optional field, bumps **minor**. A client sends the version it was built
//! against in [`Command::Hello`] and the core answers with [`HelloResult`], so
//! a mismatch is caught at connect rather than mid-turn.

use serde::{Deserialize, Serialize};

/// The protocol version this build speaks, as semver.
///
/// See the crate documentation for what a change to each field means.
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// A command a client sends to the core.
///
/// Adjacently tagged, so a value serializes to exactly the `method` and
/// `params` members of a JSON-RPC request — the envelope around them (`id`,
/// `jsonrpc`) belongs to the transport, not to this contract.
///
/// Each variant names the surface it comes from. `session/*` and `turn/answer`
/// are the routes `jan_klod_core::serve` serves today; `turn/cancel` and
/// `turn/follow-up` are new, because over REST the only way to stop a turn is
/// to drop the SSE connection and there is no way at all to steer one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Command {
    /// `protocol/hello` — version negotiation, sent before anything else.
    #[serde(rename = "protocol/hello")]
    Hello {
        /// The [`PROTOCOL_VERSION`] the client was built against.
        version: String,
    },
    /// `session/create` — allocate a session id (`POST /sessions`).
    #[serde(rename = "session/create")]
    SessionCreate,
    /// `session/list` — every session id with a preview (`GET /sessions`).
    #[serde(rename = "session/list")]
    SessionList,
    /// `session/get` — one session's transcript and metadata
    /// (`GET /session/:id`).
    #[serde(rename = "session/get")]
    SessionGet {
        /// Session id.
        session: String,
    },
    /// `session/message` — start a turn (`POST /session/:id/message`).
    #[serde(rename = "session/message")]
    SessionMessage {
        /// Session id.
        session: String,
        /// The user's message.
        message: String,
    },
    /// `turn/answer` — reply to a pending `ask`
    /// (`POST /session/:id/answer`).
    #[serde(rename = "turn/answer")]
    TurnAnswer {
        /// Session id.
        session: String,
        /// The answer the prompt asked for.
        answer: String,
    },
    /// `turn/cancel` — stop the running turn at the next loop boundary,
    /// finalizing with what is in hand.
    #[serde(rename = "turn/cancel")]
    TurnCancel {
        /// Session id.
        session: String,
    },
    /// `turn/follow-up` — steer the running turn without cancelling it.
    #[serde(rename = "turn/follow-up")]
    TurnFollowUp {
        /// Session id.
        session: String,
        /// The steering message.
        message: String,
    },
}

/// What the core answers [`Command::Hello`] with.
///
/// Carries the core's own version, so the client compares rather than assumes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    /// The protocol version the core speaks — always [`PROTOCOL_VERSION`].
    pub version: String,
}

impl Default for HelloResult {
    fn default() -> Self {
        Self {
            version: PROTOCOL_VERSION.to_owned(),
        }
    }
}
