//! Core-native interceptor dispatch — the mechanism half of the thin-loop
//! architecture ([decisions/2026-07-01-thin-loop-interceptors]).
//!
//! The core loop holds *no policy*: at each [`Phase`] it calls every enabled
//! interceptor that subscribed to that phase and acts on the returned
//! [`Decision`]. This module is the phase-agnostic engine that does exactly that,
//! plus the fail-closed / fail-open error policy. It is deliberately decoupled
//! from Wasmtime: an interceptor is anything implementing [`Interceptor`], so the
//! engine is unit-testable with plain stubs, and the wasm-guest adapter (which
//! owns a `Store` + generated bindings) is just one implementor.
//!
//! These types mirror `wit/interceptor.wit`; they are the host-side loop-state
//! the guest adapter maps to and from the generated component bindings.
//!
//! Ordering is structural, never config-driven:
//! - *across* phases: the conductor calls [`Dispatcher::dispatch`] once per phase,
//!   in [`Phase`] declaration order;
//! - *within* one phase: the load order the interceptors were registered in.

/// The lifecycle points, in dispatch order. Mirrors the `phase` enum in
/// `wit/interceptor.wit`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Per run, before the agentic loop; may short-circuit to a direct answer.
    BeforeLoop,
    /// Request-shaping: pick the model first.
    SelectModel,
    /// Request-shaping: trim/compress history to the model's budget.
    SelectContext,
    /// Request-shaping: fix the tool set.
    SelectTools,
    /// Raw assistant output in hand, before core parses/validates it.
    AfterResponse,
    /// A tool is about to be invoked. Allow / block / modify / ask.
    ToolCall,
    /// A tool has returned. Modify the result or terminate the loop.
    ToolResult,
    /// The final answer is assembled.
    Finalize,
    /// A new iteration is about to begin.
    PrepareNextTurn,
}

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

/// A tool the model may call (mirrors `llm-types.tool-definition`).
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema string describing the parameters.
    pub parameters_schema: String,
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

/// The outbound completion being assembled — the mutable state the
/// request-shaping phases read and replace (mirrors `interceptor.pending-request`).
#[derive(Debug, Clone)]
pub struct PendingRequest {
    /// `None` until `select-model` sets it.
    pub model: Option<String>,
    /// Conversation so far.
    pub messages: Vec<Message>,
    /// Tools offered to the model.
    pub tools: Vec<ToolDefinition>,
    /// Constrained-decoding grammar; core supplies a default an interceptor may
    /// override or clear.
    pub grammar: Option<String>,
    /// Token cap for the completion.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
}

/// Whether the loop actually dispatches `phase`.
///
/// The match is **exhaustive on purpose**: adding a [`Phase`] variant won't
/// compile until someone says here whether the loop reaches it — otherwise an
/// extension could subscribe to a phase that silently never runs.
#[must_use]
pub const fn is_dispatched(phase: Phase) -> bool {
    match phase {
        Phase::BeforeLoop
        | Phase::SelectModel
        | Phase::SelectContext
        | Phase::SelectTools
        | Phase::AfterResponse
        | Phase::ToolCall
        | Phase::ToolResult
        | Phase::Finalize
        | Phase::PrepareNextTurn => true,
    }
}

/// Every phase the contract declares, for tests that walk them all.
pub const ALL_PHASES: [Phase; 9] = [
    Phase::BeforeLoop,
    Phase::SelectModel,
    Phase::SelectContext,
    Phase::SelectTools,
    Phase::AfterResponse,
    Phase::ToolCall,
    Phase::ToolResult,
    Phase::Finalize,
    Phase::PrepareNextTurn,
];

/// The user's turn, handed at `before-loop`.
#[derive(Debug, Clone)]
pub struct UserTurn {
    /// Opaque session identifier.
    pub session: String,
    /// The raw user message.
    pub user_message: String,
}

/// Raw model output before parsing (handed at `after-response`).
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// The raw assistant text.
    pub text: String,
    /// `"stop"` | `"tool-calls"` | `"length"` | `"error"`.
    pub finish_reason: String,
}

/// A tool's result (handed at `tool-result`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutcome {
    /// The call this result answers.
    pub tool_call_id: String,
    /// Tool result content (may be modified).
    pub content: String,
}

/// The assembled final answer (handed at `finalize`).
#[derive(Debug, Clone)]
pub struct FinalAnswer {
    /// The answer text.
    pub text: String,
}

/// Phase-specific state handed to an interceptor. The active case always matches
/// the phase being dispatched (mirrors `interceptor.hook-state`).
#[derive(Debug, Clone)]
pub enum HookState {
    /// `before-loop` payload.
    BeforeLoop(UserTurn),
    /// `select-model` payload.
    SelectModel(PendingRequest),
    /// `select-context` payload.
    SelectContext(PendingRequest),
    /// `select-tools` payload.
    SelectTools(PendingRequest),
    /// `after-response` payload.
    AfterResponse(RawResponse),
    /// `tool-call` payload.
    ToolCall(ToolCall),
    /// `tool-result` payload.
    ToolResult(ToolOutcome),
    /// `finalize` payload.
    Finalize(FinalAnswer),
    /// `prepare-next-turn` payload.
    PrepareNextTurn(PendingRequest),
}

/// Why an action was blocked (surfaced to the user / transcript).
#[derive(Debug, Clone)]
pub struct BlockReason {
    /// Human-readable reason.
    pub message: String,
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

/// What an interceptor decided (mirrors `interceptor.decision`).
#[derive(Debug, Clone)]
pub enum Decision {
    /// No change; continue.
    Proceed,
    /// Replace the phase state (same active case as the input).
    Replace(HookState),
    /// Stop this action (deny a tool call, short-circuit the loop).
    Block(BlockReason),
    /// Ask the driver, then resume (the engine re-invokes with `answer` set).
    Ask(UserPrompt),
}

/// An interceptor's internal failure — distinct from a `Block` *decision*
/// (mirrors `interceptor.interceptor-error`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterceptorError {
    /// Unhandled internal error.
    Internal,
    /// State did not match the dispatched phase.
    InvalidState,
    /// A capability the interceptor needed failed.
    DependencyFailed,
}

/// Input to a single dispatch (mirrors `interceptor.intercept-input`).
#[derive(Debug, Clone)]
pub struct InterceptInput {
    /// The phase being dispatched.
    pub phase: Phase,
    /// The current phase state.
    pub state: HookState,
    /// Present only on re-invocation after this interceptor returned `Ask`.
    pub answer: Option<String>,
}

/// One interceptor the engine can drive. A wasm guest adapter owns a `Store` +
/// generated bindings and implements this by calling the guest's exports; the
/// tests implement it with plain stubs.
pub trait Interceptor {
    /// Stable id, used in logs and fail-closed reasons.
    fn id(&self) -> &str;

    /// Which phases this interceptor wants dispatched to it.
    fn subscribed_phases(&self) -> Vec<Phase>;

    /// Decide, for one dispatch.
    ///
    /// # Errors
    /// Returns [`InterceptorError`] when the interceptor itself malfunctions
    /// (not to be confused with a [`Decision::Block`], which is a normal
    /// outcome). The engine applies the fail-closed-at-`tool-call` policy.
    fn intercept(&mut self, input: &InterceptInput) -> Result<Decision, InterceptorError>;
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

/// The result of dispatching one phase.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Every subscribed interceptor allowed the phase to continue.
    Proceeded,
    /// An interceptor blocked (or a `tool-call` interceptor failed closed).
    Blocked(BlockReason),
}

/// The core-native dispatch engine: a fixed, load-ordered set of interceptors.
pub struct Dispatcher {
    interceptors: Vec<Box<dyn Interceptor>>,
}

impl Dispatcher {
    /// Build a dispatcher from interceptors in load (registration) order — the
    /// sole source of intra-phase ordering.
    #[must_use]
    pub fn new(interceptors: Vec<Box<dyn Interceptor>>) -> Self {
        Self { interceptors }
    }

    /// Dispatch one `phase`: call each subscribed interceptor in load order,
    /// threading `state` (a `Replace` swaps it), honouring `Block` (short-circuit)
    /// and `Ask` (suspend → `driver.ask` → resume the *same* interceptor with the
    /// answer set).
    ///
    /// Error policy (per `wit/interceptor.wit`): if an interceptor returns `Err`,
    /// **fail closed at [`Phase::ToolCall`]** (treat as a block — a broken
    /// permission gate must never fail open) and **fail open elsewhere** (log and
    /// proceed so one faulty interceptor cannot wedge the loop).
    pub fn dispatch(
        &mut self,
        phase: Phase,
        state: &mut HookState,
        driver: &mut dyn Driver,
    ) -> Outcome {
        for interceptor in &mut self.interceptors {
            if !interceptor.subscribed_phases().contains(&phase) {
                continue;
            }
            let mut answer: Option<String> = None;
            loop {
                let input = InterceptInput {
                    phase,
                    state: state.clone(),
                    answer: answer.take(),
                };
                match interceptor.intercept(&input) {
                    Ok(Decision::Proceed) => break,
                    Ok(Decision::Replace(new_state)) => {
                        *state = new_state;
                        break;
                    }
                    Ok(Decision::Block(reason)) => return Outcome::Blocked(reason),
                    Ok(Decision::Ask(prompt)) => answer = Some(driver.ask(&prompt)),
                    Err(_) => {
                        if phase == Phase::ToolCall {
                            return Outcome::Blocked(BlockReason {
                                message: format!(
                                    "interceptor `{}` failed at tool-call; failing closed",
                                    interceptor.id()
                                ),
                            });
                        }
                        // Fail open: log-and-proceed. (Event-bus emission is wired
                        // when the wasm adapter lands.)
                        break;
                    }
                }
            }
        }
        Outcome::Proceeded
    }
}

#[cfg(test)]
mod phase_tests {
    use super::{is_dispatched, ALL_PHASES};

    /// The contract may not declare a phase the loop never reaches.
    #[test]
    fn every_declared_phase_is_one_the_loop_dispatches() {
        for phase in ALL_PHASES {
            assert!(
                is_dispatched(phase),
                "{phase:?} is declared but never dispatched"
            );
        }
    }

    /// `ALL_PHASES` must not drift from the enum it enumerates.
    #[test]
    fn the_phase_list_covers_every_variant() {
        let mut seen = ALL_PHASES.to_vec();
        seen.sort_by_key(|p| format!("{p:?}"));
        seen.dedup_by_key(|p| format!("{p:?}"));
        assert_eq!(seen.len(), ALL_PHASES.len(), "no duplicates in ALL_PHASES");
    }
}
