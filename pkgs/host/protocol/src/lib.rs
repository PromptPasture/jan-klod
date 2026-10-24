//! Versioned wire contract shared by every client: commands and notifications.

pub mod contributions;
pub mod intercept;
pub mod jsonrpc;
pub mod turn;

use serde::{Deserialize, Serialize};

/// Protocol version this build speaks; breaking changes bump major, additions minor.
pub const PROTOCOL_VERSION: &str = "0.2.0";

/// Every `method` a [`Command`] can carry, so a transport can tell bad params from unknown method.
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

/// Whether `client` can talk to `core`: same major, and same minor while major is `0`.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Command {
    /// Version negotiation, sent before anything else.
    #[serde(rename = "protocol/hello")]
    Hello {
        /// The [`PROTOCOL_VERSION`] the client was built against.
        version: String,
    },
    /// Allocates a session id.
    #[serde(rename = "session/create")]
    SessionCreate,
    /// Lists every session id with a preview.
    #[serde(rename = "session/list")]
    SessionList,
    /// Returns one session's transcript.
    #[serde(rename = "session/get")]
    SessionGet {
        /// Session id.
        session: String,
    },
    /// Starts a new session from a prefix of an existing one.
    #[serde(rename = "session/fork")]
    SessionFork {
        /// The session to fork from.
        session: String,
        /// Last parent event to copy, inclusive; the child's log is renumbered from 1.
        #[serde(rename = "at-seq")]
        at_seq: u64,
    },
    /// Starts a turn.
    #[serde(rename = "session/message")]
    SessionMessage {
        /// Session id.
        session: String,
        /// The user's message.
        message: String,
    },
    /// Replies to a pending `ask`.
    #[serde(rename = "turn/answer")]
    TurnAnswer {
        /// Session id.
        session: String,
        /// The answer the prompt asked for.
        answer: String,
    },
    /// Stops the running turn at the next loop boundary, finalizing with what is in hand.
    #[serde(rename = "turn/cancel")]
    TurnCancel {
        /// Session id.
        session: String,
    },
    /// Steers the running turn without cancelling it.
    #[serde(rename = "turn/follow-up")]
    TurnFollowUp {
        /// Session id.
        session: String,
        /// The steering message.
        message: String,
    },
    /// Runs an extension-contributed command or form; routed to the declaring extension, not a session command.
    #[serde(rename = "surface/invoke")]
    SurfaceInvoke {
        /// The declaring extension's id (`Contributions::extension`).
        extension: String,
        /// The contributed `name`.
        name: String,
        /// Gathered by the client; the extension refuses undeclared ones.
        arguments: Vec<ArgumentValue>,
    },
}

/// Every SSE `event:` kind the projection can emit, sorted; not the [`Notification`] method names.
pub const SSE_FRAME_KINDS: [&str; 7] = [
    "delta",
    "done",
    "error",
    "prompt",
    "tool",
    "tool-result",
    "warning",
];

/// Something the core reports to a client unprompted; the SSE projection uses its own names for some (see [`SSE_FRAME_KINDS`]), held together by a compatibility test.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "method", content = "params")]
pub enum Notification {
    /// A chunk of assistant text.
    #[serde(rename = "text-delta")]
    TextDelta {
        /// The chunk.
        text: String,
    },
    /// A tool is about to run, after the `tool-call` gate allowed it.
    #[serde(rename = "tool-invoked")]
    ToolInvoked {
        /// Call id, matched by the `tool-result` that answers it.
        id: String,
        /// Tool name.
        name: String,
        /// JSON-encoded arguments.
        arguments: String,
    },
    /// A tool returned.
    #[serde(rename = "tool-result")]
    ToolResult {
        /// The call this answers.
        id: String,
        /// Result content.
        content: String,
        /// Whether the call failed (denied, trapped, tool-reported error, or no such tool).
        failed: bool,
    },
    /// A non-fatal notice, e.g. a provider fallback.
    #[serde(rename = "warning")]
    Warning {
        /// What happened.
        message: String,
    },
    /// The turn finished, with the authoritative answer.
    #[serde(rename = "done")]
    Done {
        /// Final answer text.
        answer: String,
        /// Whether the agentic path ran.
        agentic: bool,
    },
    /// The turn is blocked on the user; unanswered, it takes `default`.
    #[serde(rename = "ask")]
    Ask {
        /// Which session is blocked.
        session: String,
        /// The question to surface.
        question: String,
        /// Empty means free text; non-empty means choose one.
        options: Vec<String>,
        /// Used when the client cannot prompt, or does not answer in time.
        default: String,
    },
    /// A command this surface could not serve, or a turn that failed; for surfaces with no response id.
    #[serde(rename = "error")]
    Error {
        /// What went wrong, as the user should see it.
        message: String,
    },
    /// Everything every enabled extension contributes; sent at connect and whenever the set changes.
    #[serde(rename = "surface/contributions")]
    SurfaceContributions {
        /// One entry per contributing extension, in load order; may be empty.
        extensions: Vec<Contributions>,
    },
    /// This session's transcript changed.
    #[serde(rename = "session/updated")]
    SessionUpdated {
        /// Session id.
        session: String,
        /// First 80 characters of the session's first user message.
        preview: String,
    },
}

/// What the core answers [`Command::Hello`] with.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptMessage {
    /// Log position; what [`Command::SessionFork`] takes as `at_seq`. Sparse, as non-message events consume a seq.
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

/// A short piece of read-only state an extension wants visible.
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
    /// Whether the contribution set changed.
    #[serde(rename = "contributions-changed")]
    pub contributions_changed: bool,
}
