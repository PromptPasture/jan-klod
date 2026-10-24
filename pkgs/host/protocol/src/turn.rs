//! The shared turn vocabulary (#180): what a turn is made of.
//!
//! Lives here, below the loop, so `jk-session` can encode [`Event`]s without a dependency cycle.
//! No serde derives: the log and the wire ([`Notification`](crate::Notification)) each have their own encoding.

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
    /// Whether the call failed (#162). Not in `wit/interceptor.wit`'s
    /// `tool-outcome`: the wasm adapter restores it across a guest replace.
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

    /// A follow-up user message to inject instead of ending the turn (steering).
    /// Called when a turn would otherwise finish; `None` (the default) ends it.
    fn follow_up(&mut self) -> Option<String> {
        None
    }
}

/// Event emitted during a turn, streamed via [`EventSink`].
/// `TextDelta` is a non-authoritative preview; `Done` is authoritative.
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

/// Whether the loop continues after an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep going.
    Continue,
    /// Cancel the turn at the next boundary (finalize with what's in hand).
    Stop,
}

/// Sink for the conductor's [`Event`]s; synchronous, on the turn's thread (`!Send`).
pub trait EventSink {
    /// Handle one event; return [`Flow::Stop`] to cancel the turn.
    fn emit(&mut self, event: &Event) -> Flow;
}
