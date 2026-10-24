//! Versioned wire contract shared by every client: named commands and notifications.
//! No I/O and no policy; [`jsonrpc`] frames a [`Command`], the core and its interceptors decide what is allowed.
//! [`turn`], [`intercept`] and [`contributions`] sit here so every crate above shares one vocabulary.

pub mod contributions;
pub mod intercept;
pub mod jsonrpc;
pub mod turn;

use serde::{Deserialize, Serialize};

/// The protocol version this build speaks, as semver.
///
/// Removing or renaming a command, or removing a field, bumps major; adding a
/// command or an optional field bumps minor. See [`compatible`].
pub const PROTOCOL_VERSION: &str = "0.2.0";

/// Every `method` a [`Command`] can carry.
///
/// Lets a transport tell `invalid params` (known method, bad params) from
/// `method not found`, which serde reports as one error. `wire.rs` checks this
/// against the exhaustive `match` over [`Command`] in both directions.
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
    "surface/invoke",
];

/// Whether a client built against `client` can talk to a core speaking `core`.
///
/// Same major, and while major is `0` the same minor too. A malformed version is
/// incompatible.
///
/// Deliberately duplicated in `jk_wasm::manifest::api_compatible`: the WIT and
/// protocol version lines are independent. See
/// [Contracts](../../../../../docs/concepts/contracts.md).
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
/// Adjacently tagged so it serializes to the `method`/`params` members of a
/// JSON-RPC request; [`jsonrpc::Request`] flattens it, which needs a map.
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
    /// Declared before any transport carried it, so non-Rust clients can generate from the schema.
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
    /// `surface/invoke` — run a command or form an extension contributed
    /// (`wit/client-surface.wit`).
    ///
    /// Not a session command: the host routes it to the declaring extension
    /// and answers with [`SurfaceInvokeResult`].
    #[serde(rename = "surface/invoke")]
    SurfaceInvoke {
        /// The extension that declared it, as `Contributions::extension`; with
        /// `name` it identifies a contribution.
        extension: String,
        /// The contributed `name`.
        name: String,
        /// Whatever the client gathered; the extension refuses undeclared ones.
        arguments: Vec<ArgumentValue>,
    },
}

/// Every `event:` kind the SSE projection can emit, sorted.
///
/// Lives here so producer and consumer are checked against one list:
/// `pkgs/host/core/tests/protocol_events.rs` checks the core emits exactly
/// these, and `ui/tests/parse_frame.rs` checks a client handles each (#81).
/// Not the [`Notification`] method names.
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
/// Tagged like [`Command`]; a transport sends it as a JSON-RPC request with no `id`.
///
/// The first five mirror [`turn::Event`]. `Ask` is the pending prompt answered by
/// [`Command::TurnAnswer`]; `Error` is a failed turn or unservable command;
/// `SessionUpdated` saves a client from polling.
///
/// The SSE projection keeps its own names: `delta` for `text-delta`, `tool` for
/// `tool-invoked`, `prompt` for `ask`; the rest match (see [`SSE_FRAME_KINDS`]).
/// Do not align either side; a compatibility test holds them together.
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
        /// Whether the call failed: a permission denial, a trap, a tool-reported
        /// error, or no such tool.
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
        /// Which session is blocked; the answer is a separate request, so it must name it.
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
    /// Where there is an id to answer, a transport sends a
    /// [`jsonrpc::Response::error`] instead and not this, so a failure is not
    /// reported twice. This is for surfaces with no id, such as SSE.
    #[serde(rename = "error")]
    Error {
        /// What went wrong, as the user should see it.
        message: String,
    },
    /// `surface/contributions` — what every enabled extension contributes to
    /// Sent at connect and again whenever the set changes. One notification, not
    /// a delta, since a client needs the whole set both times. Clients may ignore it.
    #[serde(rename = "surface/contributions")]
    SurfaceContributions {
        /// One entry per contributing extension, in load order; may be empty.
        extensions: Vec<Contributions>,
    },
    /// `session/updated` — this session's transcript changed.
    #[serde(rename = "session/updated")]
    SessionUpdated {
        /// Session id; the REST list projection spells it `id`.
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
/// Declared here so non-Rust clients can generate it and learn the seq to fork at (#106).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    /// The log position this was projected from; what [`Command::SessionFork`]
    /// takes as `at_seq`, inclusive.
    ///
    /// Sparse: events that project to no message still consume a seq.
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

/// One argument a contributed command takes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Argument {
    /// Machine name, unique within the command.
    pub name: String,
    /// One line, shown to the user. Data, not markup.
    pub description: String,
    /// Whether the command can run without it.
    pub required: bool,
}

/// Something a user can invoke: a slash command, a menu entry, a button.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceCommand {
    /// Machine name, unique within the extension.
    pub name: String,
    /// Short label. Data, not markup.
    pub title: String,
    /// One line of help. Data, not markup.
    pub description: String,
    /// In declaration order.
    pub arguments: Vec<Argument>,
}

/// A short piece of state an extension wants visible. Read-only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusItem {
    /// Machine name, unique within the extension.
    pub name: String,
    /// The text to show, already short. Data, not markup.
    pub text: String,
    /// One line of detail, for a client with room for it.
    pub detail: String,
}

/// One field of a contributed form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Field {
    /// Machine name, unique within the form.
    pub name: String,
    /// Label. Data, not markup.
    pub label: String,
    /// Empty means free text; non-empty means choose one.
    pub options: Vec<String>,
    /// Used when the client cannot prompt, or the user declines to fill it.
    #[serde(rename = "default")]
    pub default_value: String,
}

/// A small set of fields gathered in one go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceForm {
    /// Machine name, unique within the extension.
    pub name: String,
    /// Short label. Data, not markup.
    pub title: String,
    /// In declaration order.
    pub fields: Vec<Field>,
}

/// Everything one extension contributes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contributions {
    /// The declaring extension's instance id, e.g. `interceptor.system`.
    pub extension: String,
    /// Commands, in declaration order.
    pub commands: Vec<SurfaceCommand>,
    /// Status items, in declaration order.
    #[serde(rename = "status-items")]
    pub status_items: Vec<StatusItem>,
    /// Forms, in declaration order.
    pub forms: Vec<SurfaceForm>,
}

/// An argument as the client supplied it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgumentValue {
    /// The declared argument's name.
    pub name: String,
    /// What the user gave.
    pub value: String,
}

/// What the core answers [`Command::SurfaceInvoke`] with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SurfaceInvokeResult {
    /// Shown to the user. Data, not markup.
    pub text: String,
    /// Whether the contribution set changed, so a client knows the
    /// [`Notification::SurfaceContributions`] it is about to receive is not a
    /// duplicate of the one it already has.
    #[serde(rename = "contributions-changed")]
    pub contributions_changed: bool,
}
