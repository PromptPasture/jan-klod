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
//! * **No I/O.** No sockets, no pipes, no read loop. A [`Command`] carries the
//!   `method`/`params` pair; [`jsonrpc`] wraps it in the frame both transports
//!   send. Which is a correction: this crate first excluded framing too, on the
//!   grounds that it belonged to the transport. That holds for one transport and
//!   fails for two parties — see the [`jsonrpc`] module documentation for what
//!   changed the answer.
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
//! a mismatch is caught at connect rather than mid-turn. [`compatible`] is what
//! decides — and while this version is `0.x`, a differing **minor** is a
//! refusal too.

pub mod jsonrpc;

use serde::{Deserialize, Serialize};

/// The protocol version this build speaks, as semver.
///
/// See the crate documentation for what a change to each field means.
pub const PROTOCOL_VERSION: &str = "0.1.0";

/// Every `method` a [`Command`] can carry.
///
/// A transport needs this to tell two failures apart, and a client's dispatcher
/// can use it too. When `{"method":"session/get"}` arrives without its
/// `params`, deserializing [`Command`] fails; so does
/// `{"method":"session/destroy"}`. One is JSON-RPC's `invalid params` and the
/// other is `method not found`, and serde reports both as one error — the
/// method name is what distinguishes them.
///
/// This is a hand-written list, which is exactly the kind of thing that falls
/// behind the enum beside it. `wire.rs` compares it against the exhaustive
/// `match` over [`Command`] in both directions, so a command added without a
/// line here fails the tests rather than becoming a `method not found` that
/// lies.
pub const COMMAND_METHODS: &[&str] = &[
    "protocol/hello",
    "session/create",
    "session/list",
    "session/get",
    "session/fork",
    "session/message",
    "turn/answer",
    "turn/cancel",
    "turn/follow-up",
];

/// Whether a client built against `client` can talk to a core speaking `core`.
///
/// Same major, and — while the major is `0` — the same minor too. The second
/// clause is the one that decides anything today, because [`PROTOCOL_VERSION`]
/// is `0.1.0`: a major-only check would wave a `0.9` client through to a `0.1`
/// core and call it negotiated. A version that does not parse is incompatible,
/// since guessing at a malformed version is how a check becomes decoration.
///
/// # This rule is deliberately duplicated
///
/// `jan_klod_core::manifest::api_compatible` applies the same test to the WIT
/// `jan-klod:interfaces` version, and this is not a copy waiting to be
/// deduplicated. The two version *lines* are independent — a WIT change need
/// not touch a command, and a new command need not touch WIT — so sharing one
/// predicate would mean one of them dragging the other to a decision it did not
/// make. What is shared is the reasoning, which is written down in
/// [Contracts](../../../../docs/concepts/contracts.md).
#[must_use]
pub fn compatible(core: &str, client: &str) -> bool {
    let parts = |v: &str| -> Option<(u64, u64)> {
        let mut it = v.split('.');
        let major = it.next()?.parse().ok()?;
        let minor = it.next()?.parse().ok()?;
        Some((major, minor))
    };
    match (parts(core), parts(client)) {
        (Some((core_major, core_minor)), Some((their_major, their_minor))) => {
            core_major == their_major && (core_major != 0 || core_minor == their_minor)
        }
        _ => false,
    }
}

/// A command a client sends to the core.
///
/// Adjacently tagged, so a value serializes to exactly the `method` and
/// `params` members of a JSON-RPC request and nothing more. That is not a
/// stylistic choice: [`jsonrpc::Request`] flattens a command into the frame,
/// and `#[serde(flatten)]` only works over a value that serializes as a map,
/// which is what adjacent tagging produces — internal or external tagging would
/// not.
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
    /// `session/fork` — start a new session from a prefix of this one
    /// (`POST /session/:id/fork`).
    ///
    /// Contract only for now: no transport carries a command yet, so the REST
    /// route is what a client actually calls. It is declared here anyway
    /// because the schema is what non-Rust clients generate from, and a route
    /// that exists but is absent from the contract is how the two drift.
    #[serde(rename = "session/fork")]
    SessionFork {
        /// The session to fork from.
        session: String,
        /// The last event of the parent's log to copy, inclusive. The child's
        /// own log is renumbered from 1.
        #[serde(rename = "at-seq")]
        at_seq: u64,
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

/// Something the core reports to a client, unprompted.
///
/// Tagged like [`Command`], so a transport wraps both the same way — a
/// notification is a JSON-RPC request with no `id`, which is exactly what
/// "no reply expected" means there.
///
/// The first five mirror `jan_klod_core::conductor::Event` one for one. The
/// rest do not come from an event at all: `Ask` is the pending prompt an
/// interceptor blocks a turn on, answered by [`Command::TurnAnswer`]; `Error`
/// is a turn that failed or a command that could not be served;
/// `SessionUpdated` reports a session whose transcript moved, so a client
/// listing sessions does not have to poll.
///
/// # These are not the SSE event names
///
/// The SSE projection in `jan_klod_core::serve` predates this contract and
/// keeps its own names: `delta` for `text-delta`, `tool` for `tool-invoked`,
/// and `prompt` for `ask`. `tool-result`, `warning`, `done` and `error` match.
/// Do not "fix" either side to agree with the other — the projection is allowed
/// its own spelling, and the compatibility test is what holds them together.
/// `ToolInvoked` is also a strict superset: the SSE `tool` frame carries only
/// `id` and `name`, dropping the call's `arguments`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Notification {
    /// `text-delta` — a chunk of assistant text (one per completion in v1, not
    /// per token).
    #[serde(rename = "text-delta")]
    TextDelta {
        /// The chunk.
        text: String,
    },
    /// `tool-invoked` — a tool is about to run, the `tool-call` gate having
    /// allowed it.
    #[serde(rename = "tool-invoked")]
    ToolInvoked {
        /// Call id, matched by the `tool-result` that answers it.
        id: String,
        /// Tool name.
        name: String,
        /// JSON-encoded arguments.
        arguments: String,
    },
    /// `tool-result` — a tool returned.
    #[serde(rename = "tool-result")]
    ToolResult {
        /// The call this answers.
        id: String,
        /// Result content.
        content: String,
    },
    /// `warning` — a non-fatal notice, e.g. a provider fallback.
    #[serde(rename = "warning")]
    Warning {
        /// What happened.
        message: String,
    },
    /// `done` — the turn finished, with the authoritative answer.
    #[serde(rename = "done")]
    Done {
        /// Final answer text.
        answer: String,
        /// Whether the agentic path ran.
        agentic: bool,
    },
    /// `ask` — the turn is blocked on the user. Reply with
    /// [`Command::TurnAnswer`]; leaving it unanswered takes `default`.
    #[serde(rename = "ask")]
    Ask {
        /// Which session is blocked. The SSE `prompt` frame carries this too:
        /// the client answers over a separate request, so the answer has to say
        /// what it is answering.
        session: String,
        /// The question to surface.
        question: String,
        /// Empty means free text; non-empty means choose one.
        options: Vec<String>,
        /// Used when the client cannot prompt, or does not answer in time.
        default: String,
    },
    /// `error` — a command this surface could not serve, or a turn that failed.
    ///
    /// One notification for both, because the SSE `error` frame it has to stay
    /// compatible with does not distinguish them either. **Slice 13b made that
    /// call:** a command that could not be served is answered with a
    /// [`jsonrpc::Response::error`] against its id, because a client waiting on
    /// an id has to be released; a failed turn has no command to answer, so it
    /// arrives here as a notification.
    #[serde(rename = "error")]
    Error {
        /// What went wrong, as the user should see it.
        message: String,
    },
    /// `session/updated` — this session's transcript changed.
    #[serde(rename = "session/updated")]
    SessionUpdated {
        /// Session id. Named for the field the commands use; the REST list
        /// projection spells the same value `id`.
        session: String,
        /// First 80 characters of the session's first user message.
        preview: String,
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
