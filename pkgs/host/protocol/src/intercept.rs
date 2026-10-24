//! The interceptor contract, the [`Completer`]/[`ToolInvoker`] backends, and the [`Dispatcher`] engine.
//!
//! Host-side mirror of `wit/interceptor.wit`, below both the wasm adapters and the loop (#181).
//! Ordering is structural: phases in [`Phase`] order, interceptors in load order within a phase.

use crate::contributions::{ArgumentValue, Contributions, InvokeError, InvokeOutcome};
use crate::turn::{Driver, Message, ToolCall, ToolOutcome, UserPrompt};

/// The lifecycle points, in dispatch order. Mirrors `wit/interceptor.wit`.
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
/// Exhaustive on purpose: a new [`Phase`] won't compile until this says whether
/// the loop reaches it, so no extension can subscribe to a phase that never runs.
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
    /// Optional principal identifying the user (from session store).
    pub principal: Option<String>,
}

/// Raw model output before parsing (handed at `after-response`).
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// The raw assistant text.
    pub text: String,
    /// `"stop"` | `"tool-calls"` | `"length"` | `"error"`.
    pub finish_reason: String,
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

/// One interceptor the engine can drive; a wasm guest adapter or a test stub.
pub trait Interceptor {
    /// Stable id, used in logs and fail-closed reasons.
    fn id(&self) -> &str;

    /// Which phases this interceptor wants dispatched to it.
    fn subscribed_phases(&self) -> Vec<Phase>;

    /// Decide, for one dispatch.
    ///
    /// # Errors
    /// Returns [`InterceptorError`] when the interceptor itself malfunctions
    /// (not [`Decision::Block`], a normal outcome).
    fn intercept(&mut self, input: &InterceptInput) -> Result<Decision, InterceptorError>;

    /// What this extension contributes to a client's interface, if anything.
    ///
    /// Defaults to nothing: most interceptors export no `client-surface`.
    fn contributions(&mut self) -> Option<crate::contributions::Contributions> {
        None
    }

    /// Run a contribution this extension declared.
    ///
    /// # Errors
    /// [`crate::contributions::InvokeError::Unknown`] when nothing of that name
    /// is contributed (the default).
    fn invoke_contribution(
        &mut self,
        _name: &str,
        _arguments: &[crate::contributions::ArgumentValue],
    ) -> Result<crate::contributions::InvokeOutcome, crate::contributions::InvokeError> {
        Err(crate::contributions::InvokeError::Unknown)
    }
}

/// One model completion: the assistant text plus any tool calls it emitted.
#[derive(Debug, Clone, Default)]
pub struct Completion {
    /// Assistant text (may be empty when the model only emitted tool calls).
    pub text: String,
    /// Tool calls the model wants run before it continues.
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped (provider spelling), empty if not provided. Kept
    /// because `length` means a truncated answer; see [`TRUNCATED`].
    pub finish_reason: String,
}

/// Finish-reasons meaning the model ran out of room (`length` is the
/// OpenAI-compatible spelling; raw `max_tokens` is also accepted).
pub const TRUNCATED: [&str; 2] = ["length", "max_tokens"];

impl Completion {
    /// Whether the model stopped because it ran out of room.
    #[must_use]
    pub fn was_truncated(&self) -> bool {
        TRUNCATED.contains(&self.finish_reason.as_str())
    }
}

/// Completion backend, implemented by a provider extension; the fallback chain tries them in order.
pub trait Completer {
    /// Stable id, used in fallback diagnostics.
    fn id(&self) -> &str;

    /// Complete the assembled request.
    ///
    /// # Errors
    /// Returns a human-readable provider failure; the conductor falls back to the
    /// next completer.
    fn complete(&mut self, request: &PendingRequest) -> Result<Completion, String>;
}

/// Result of invoking a tool: content and whether it failed (#162).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocation {
    /// The tool's result, or a human-readable description of why it failed.
    pub content: String,
    /// Whether the call failed.
    pub failed: bool,
}

/// A tool backend: routes a tool call to the extension that implements it.
pub trait ToolInvoker {
    /// Invoke `call`. `None` means no tool matched; the conductor reports a
    /// failed result rather than aborting the turn.
    fn invoke(&mut self, call: &ToolCall) -> Option<ToolInvocation>;

    /// Invoke `call`, letting the tool ask the user first if it can (#216).
    ///
    /// Defaults to [`Self::invoke`], ignoring the driver. The driver is passed,
    /// not stored, so a tool cannot ask outside a tool call.
    fn invoke_asking(
        &mut self,
        call: &ToolCall,
        _driver: &mut dyn Driver,
    ) -> Option<ToolInvocation> {
        self.invoke(call)
    }

    /// Tell the fleet which session this turn's calls belong to (#215).
    ///
    /// Defaults to a no-op. A fleet that needs a session and is not given one
    /// refuses rather than falling back to shared state, so sessions never leak.
    fn bind_session(&mut self, _session: &str) {}
}

/// A [`ToolInvoker`] with no tools; every call is absent.
pub struct NoTools;
impl ToolInvoker for NoTools {
    fn invoke(&mut self, _call: &ToolCall) -> Option<ToolInvocation> {
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

/// The dispatch engine: a fixed, load-ordered set of interceptors.
pub struct Dispatcher {
    interceptors: Vec<Box<dyn Interceptor>>,
}

impl Dispatcher {
    /// Build a dispatcher from interceptors in load order, the sole source of intra-phase ordering.
    #[must_use]
    pub fn new(interceptors: Vec<Box<dyn Interceptor>>) -> Self {
        Self { interceptors }
    }

    /// What the loaded extensions contribute, in load order, skipping empty sets.
    ///
    /// Read on demand: an extension may report a change after an invocation.
    pub fn contributions(&mut self) -> Vec<Contributions> {
        self.interceptors
            .iter_mut()
            .filter_map(|interceptor| interceptor.contributions())
            .filter(|set| !set.is_empty())
            .collect()
    }

    /// Run a contribution, routed by the declaring extension's id.
    ///
    /// # Errors
    /// [`InvokeError::Unknown`] when no loaded extension
    /// has that id, or it contributes no such name.
    pub fn invoke_contribution(
        &mut self,
        extension: &str,
        name: &str,
        arguments: &[ArgumentValue],
    ) -> Result<InvokeOutcome, InvokeError> {
        self.interceptors
            .iter_mut()
            .find(|interceptor| interceptor.id() == extension)
            .ok_or(InvokeError::Unknown)?
            .invoke_contribution(name, arguments)
    }

    /// Dispatch one `phase`: call each subscribed interceptor in load order,
    /// threading `state`, honouring `Block` and `Ask` (resumes the same
    /// interceptor with the answer set).
    ///
    /// An `Err` **fails closed at [`Phase::ToolCall`]** (a broken permission gate
    /// must never fail open) and fails open elsewhere so one faulty interceptor
    /// cannot wedge the loop.
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
