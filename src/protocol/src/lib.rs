//! One versioned wire contract shared by every client.
//!
//! Today a client is whatever `jan_klod_core::serve` happens to serve — REST
//! routes plus an SSE stream — so each new client (TUI, web, editor) reads the
//! route table and drifts from the others. This crate makes that surface a
//! contract of the same rank as the WIT package: named commands, named
//! notifications, and a version that says when either changed.
//!
//! Two things are deliberately *not* here:
//!
//! * **No I/O.** No sockets, no pipes, no read loop. A [`Command`] carries the
//!   `method`/`params` pair; [`jsonrpc`] wraps it in the frame both transports
//!   send. This is a correction: this crate first excluded framing too, but
//!   that holds for one transport and fails for two parties — see the
//!   [`jsonrpc`] module documentation for why.
//! * **No policy.** These are the shapes of what clients say and what the core
//!   reports back; whether a command is *allowed* is the core's and its
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
/// A transport needs this to distinguish failures; a client dispatcher can use
/// it too. When `{"method":"session/get"}` arrives without its `params`,
/// deserializing [`Command`] fails; so does `{"method":"session/destroy"}`.
/// One is JSON-RPC's `invalid params` and the other `method not found`, and
/// serde reports both as one error — the method name is what distinguishes them.
///
/// This is hand-written and easily falls out of sync with the enum beside it.
/// `wire.rs` compares it against the exhaustive `match` over [`Command`] in
/// both directions, so a command added without a line here fails the tests
/// rather than lying as `method not found`.
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
/// Same major, and — while major is `0` — the same minor too. The second
/// clause decides today: since [`PROTOCOL_VERSION`] is `0.1.0`, a major-only
/// check would accept a `0.9` client on a `0.1` core. A malformed version is
/// incompatible; guessing at one makes the check decoration rather than truth.
///
/// # This rule is deliberately duplicated
///
/// `jan_klod_core::manifest::api_compatible` applies the same test to the WIT
/// `jan-klod:interfaces` version, and this is not a copy to deduplicate. The
/// two version *lines* are independent — a WIT change need not touch a command,
/// and vice versa — so sharing one predicate would drag one to decisions it did
/// not make. What is shared is the reasoning, written in
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
/// `params` members of a JSON-RPC request. This is not stylistic:
/// [`jsonrpc::Request`] flattens a command into the frame, and
/// `#[serde(flatten)]` only works over a value serializing as a map, which
/// adjacent tagging produces — internal or external tagging would not.
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
    /// Declared here before any transport carried it, because the schema is
    /// what non-Rust clients generate from and a route that exists but is
    /// absent from the contract is how the two drift. Slice 13b's stdio
    /// transport now serves it.
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

/// Every `event:` kind the SSE projection can emit, sorted.
///
/// Here rather than in the core because **a client cannot see the core**: this
/// crate is the only thing `jan-klod-core` and a UI both depend on, which is
/// what lets the producer and the consumer be checked against one list instead
/// of against each other's prose. Without that, a client can simply not know a
/// frame — #81, where every tool result in a healthy turn reached the user as
/// an error because `parse_frame` had never heard of `tool-result`.
///
/// Two tests hold the ends, and neither crate needs to see the other:
/// `core/tests/protocol_events.rs` proves this is exactly what
/// `serve::sse_frame`, `prompt_frame` and `error_frame` produce across every
/// `conductor::Event`, and `ui/tests/parse_frame.rs` proves a client has an
/// answer for each. Adding a frame to the core fails the first; shipping a
/// client that ignores it fails the second.
///
/// These are **not** the [`Notification`] method names — see that type's docs.
pub const SSE_FRAME_KINDS: [&str; 7] = [
    "delta",
    "done",
    "error",
    "prompt",
    "tool",
    "tool-result",
    "warning",
];

/// Something the core reports to a client, unprompted.
///
/// Tagged like [`Command`], so a transport wraps both the same way — a
/// notification is a JSON-RPC request with no `id`, the signal for
/// "no reply expected".
///
/// The first five mirror `jan_klod_core::conductor::Event`. The rest are not
/// events: `Ask` is the pending prompt an interceptor blocks a turn on,
/// answered by [`Command::TurnAnswer`]; `Error` is a turn that failed or a
/// command that could not be served; `SessionUpdated` reports a session whose
/// transcript moved, so a client listing sessions need not poll.
///
/// # These are not the SSE event names
///
/// The SSE projection in `jan_klod_core::serve` predates this contract and
/// keeps its own names: `delta` for `text-delta`, `tool` for `tool-invoked`,
/// and `prompt` for `ask`. `tool-result`, `warning`, `done` and `error` match.
/// Do not "fix" either side to agree with the other — the projection is allowed
/// its own spelling, and the compatibility test is what holds them together.
///
/// The `tool` frame used to be a strict subset of `ToolInvoked` — it carried
/// only `id` and `name`, dropping the call's `arguments` — so a REST+SSE
/// client could not render what stdio could. Fixed by #161: the frame now
/// carries every field the notification does.
///
/// Those names are [`SSE_FRAME_KINDS`], so the paragraph above is prose beside
/// the data rather than a second copy of it.
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
        /// Whether the call failed — a permission denial, a trap, an error the
        /// tool itself reported, or no tool by that name. Until #162 nothing on
        /// the wire said this; a client could only guess by matching the
        /// sentences the core happens to write into `content`, which coupled a
        /// client to wording it does not own.
        failed: bool,
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
    /// compatible with does not distinguish them either. **Slice 13b settled
    /// how a transport uses it:** where there is an id to answer, the failure
    /// is answered against it with a [`jsonrpc::Response::error`] and this
    /// notification is not sent — a client waiting on an id has to be released,
    /// and reporting the same failure twice invites a client to show it twice.
    /// So over stdio a failed *turn* is the error response to its own
    /// `session/message`. This notification is for a surface with no id to
    /// answer, which is what SSE is.
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

/// One message of a session's transcript, as [`Command::SessionGet`] returns it.
///
/// # Why the read shape is declared here at all
///
/// It was not; see [#106](https://github.com/PromptPasture/jan-klod/issues/106).
/// `session/fork` takes an event seq but `session/get` returned only
/// `{role, content}` — so the command reading a session never said where in
/// the log anything sat, and a client could fork only at a number with no way
/// to obtain it. Commands are declared in this crate ahead of any transport,
/// *because non-Rust clients generate from the schema*; their result was left
/// as untyped JSON in `session::session_payload`, reaching neither schema nor
/// generated clients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    /// The log position this message was projected from, and what
    /// [`Command::SessionFork`] takes as `at_seq` — inclusive, so forking here
    /// yields a session whose transcript ends with this message.
    ///
    /// **Sparse.** Events that project to no message — an ask, an answer, a
    /// text delta — still consume a seq, so this is a position in the log and
    /// not an index into the list.
    pub seq: u64,
    /// `system`, `user`, `assistant` or `tool`.
    pub role: String,
    /// The message text.
    pub content: String,
    /// Present only on a tool result, tying it to the call it answers.
    #[serde(rename = "tool-call-id", skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

/// What the core answers [`Command::SessionGet`] with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionGetResult {
    /// The session's id, echoed back.
    pub id: String,
    /// The transcript, oldest first.
    pub messages: Vec<TranscriptMessage>,
}
