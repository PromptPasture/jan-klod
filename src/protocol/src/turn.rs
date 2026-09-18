//! The shared turn vocabulary — what a turn is made of, named once (#180).
//!
//! These types were in `jan-klod-core`, beside the loop that produces them.
//! That worked while one crate held everything. It stops working at the first
//! crate boundary: the event log encodes an [`Event`] and rebuilds a
//! transcript out of [`Message`]s, so `jk-session` needs this vocabulary, and
//! `jk-session` sits *below* the loop. Leaving the types with the loop would
//! make `jk-session` depend on `jk-agent`, which depends on `jk-session` — a
//! cycle cargo rejects before anything runs.
//!
//! So they live here, in the crate that already has no dependencies but
//! serde. The direction works for everyone: session, wasm and agent all
//! depend on the protocol, and none of them on each other's vocabulary.
//!
//! # Why *this* crate rather than an eighth one
//!
//! Because the wire notifications and the log records are **two projections
//! of one turn**. A [`Notification`](crate::Notification) is what a client is
//! told happened; an [`Event`] is what the log remembers happening. Putting
//! them in separate crates would leave two places asserting the shapes of one
//! thing, which is how they drift. `protocol_events.rs` in `jan-klod-host`
//! already treats them as two views, and now they are two views declared side
//! by side.
//!
//! The types themselves carry no serde derives: the log has its own versioned
//! envelope and the wire has [`Notification`](crate::Notification), and a
//! third encoding derived here would be a format nobody asked for.

/// Role of a message in the conversation (mirrors `llm-types.role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// System / instruction message.
    System,
    /// End-user message.
    User,
    /// Model message.
    Assistant,
    /// Tool-result message.
    Tool,
}

/// A single conversation message (mirrors `llm-types.message`).
#[derive(Debug, Clone)]
pub struct Message {
    /// Who authored the message.
    pub role: Role,
    /// The message text.
    pub content: String,
    /// Non-empty only when `role == Tool`.
    pub tool_call_id: Option<String>,
}

/// A tool call emitted by the model (mirrors `llm-types.tool-call`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    /// Unique id for this call.
    pub id: String,
    /// Tool name being invoked.
    pub name: String,
    /// JSON-encoded arguments.
    pub arguments: String,
}

/// A tool's result (handed at `tool-result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    /// The call this result answers.
    pub tool_call_id: String,
    /// Tool result content (may be modified).
    pub content: String,
    /// Whether the call failed (#162). **Not** part of `wit/interceptor.wit`'s
    /// `tool-outcome` record — a wasm `tool-result` interceptor may replace
    /// `content`, but whether it failed isn't its call to make, so the wasm
    /// adapter (`interceptor_host.rs`) restores this field across such a
    /// replace rather than letting the guest set it.
    pub failed: bool,
}

/// A question routed through the loop to the attached driver.
#[derive(Debug, Clone)]
pub struct UserPrompt {
    /// The question to surface.
    pub question: String,
    /// Empty = free-text; non-empty = choose one.
    pub options: Vec<String>,
    /// Used when the driver cannot prompt (headless).
    pub default_answer: String,
}

/// The attached driver (TUI, chat, api-*) that answers an `Ask`.
pub trait Driver {
    /// Surface `prompt` and return the user's answer (or a default when headless).
    fn ask(&mut self, prompt: &UserPrompt) -> String;

    /// A follow-up user message to inject instead of ending the turn — the driver's
    /// **steering** hook. The loop calls this when a turn would otherwise finish (no
    /// pending tool calls): `Some(msg)` injects `msg` and runs another cycle; `None`
    /// ends the turn. Default: none.
    fn follow_up(&mut self) -> Option<String> {
        None
    }
}

/// Event emitted during a turn. Streamed via [`EventSink`] to drivers (TUI, REST SSE).
/// `text-delta` is non-authoritative preview; terminal `Done` is authoritative
/// (after-response/finalize may rewrite).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// A chunk of assistant text (one per completion in v1, not per token).
    TextDelta(String),
    /// A tool is about to run (after the `tool-call` gate allowed it).
    ToolInvoked(ToolCall),
    /// A tool returned (post `tool-result`).
    ToolResult(ToolOutcome),
    /// A non-fatal notice (e.g. provider fallback, a malformed-output retry).
    Warning(String),
    /// The turn finished with the authoritative answer.
    Done {
        /// Final answer text.
        text: String,
        /// Whether the agentic path ran.
        agentic: bool,
    },
}

/// Whether loop continues after an event. Sink returns [`Flow::Stop`] to cancel
/// at next boundary (e.g., disconnected SSE client, explicit stop button).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep going.
    Continue,
    /// Cancel the turn at the next boundary (finalize with what's in hand).
    Stop,
}

/// Sink for conductor to push [`Event`]s; synchronous, on turn's thread (sync loop, `!Send`).
/// Return [`Flow::Stop`] to cancel turn.
pub trait EventSink {
    /// Handle one event; return [`Flow::Stop`] to cancel the turn.
    fn emit(&mut self, event: &Event) -> Flow;
}
