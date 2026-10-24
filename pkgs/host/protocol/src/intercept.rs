//! Host-side mirror of `wit/interceptor.wit`: the interceptor contract, backends and [`Dispatcher`].

use crate::contributions::{ArgumentValue, Contributions, InvokeError, InvokeOutcome};
use crate::turn::{Driver, Message, ToolCall, ToolOutcome, UserPrompt};

/// The lifecycle points, in dispatch order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Before the agentic loop; may short-circuit to a direct answer.
    BeforeLoop,
    /// Picks the model.
    SelectModel,
    /// Trims history to the model's budget.
    SelectContext,
    /// Fixes the tool set.
    SelectTools,
    /// Raw assistant output, before core parses it.
    AfterResponse,
    /// A tool is about to run: allow, block, modify or ask.
    ToolCall,
    /// A tool has returned; may modify the result or end the loop.
    ToolResult,
    /// The final answer is assembled.
    Finalize,
    /// A new iteration is about to begin.
    PrepareNextTurn,
}

/// A tool the model may call.
#[derive(Debug, Clone)]
pub struct ToolDefinition {
    /// Tool name.
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// JSON Schema string describing the parameters.
    pub parameters_schema: String,
}

/// The outbound completion being assembled, which the request-shaping phases replace.
#[derive(Debug, Clone)]
pub struct PendingRequest {
    /// `None` until `select-model` sets it.
    pub model: Option<String>,
    /// Conversation so far.
    pub messages: Vec<Message>,
    /// Tools offered to the model.
    pub tools: Vec<ToolDefinition>,
    /// Constrained-decoding grammar; core's default may be overridden or cleared.
    pub grammar: Option<String>,
    /// Token cap for the completion.
    pub max_tokens: Option<u32>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
}

/// Whether the loop dispatches `phase`; exhaustive so a new [`Phase`] must be classified.
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

/// Every phase the contract declares.
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
    /// Principal identifying the user, from the session store.
    pub principal: Option<String>,
}

/// Raw model output before parsing, handed at `after-response`.
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

/// Phase-specific state handed to an interceptor; the active case matches the dispatched phase.
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

/// Why an action was blocked, surfaced to the user.
#[derive(Debug, Clone)]
pub struct BlockReason {
    /// Human-readable reason.
    pub message: String,
}

/// What an interceptor decided.
#[derive(Debug, Clone)]
pub enum Decision {
    /// No change; continue.
    Proceed,
    /// Replace the phase state (same active case as the input).
    Replace(HookState),
    /// Stop this action (deny a tool call, short-circuit the loop).
    Block(BlockReason),
    /// Ask the driver, then re-invoke with `answer` set.
    Ask(UserPrompt),
}

/// An interceptor's internal failure, distinct from a `Block` decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterceptorError {
    /// Unhandled internal error.
    Internal,
    /// State did not match the dispatched phase.
    InvalidState,
    /// A capability the interceptor needed failed.
    DependencyFailed,
}

/// Input to a single dispatch.
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
    /// [`InterceptorError`] when the interceptor itself malfunctions, not for a block.
    fn intercept(&mut self, input: &InterceptInput) -> Result<Decision, InterceptorError>;

    /// What this extension contributes to a client's interface; nothing by default.
    fn contributions(&mut self) -> Option<crate::contributions::Contributions> {
        None
    }

    /// Run a contribution this extension declared.
    ///
    /// # Errors
    /// `Unknown` when nothing of that name is contributed (the default).
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
    /// Assistant text; may be empty when only tool calls were emitted.
    pub text: String,
    /// Tool calls the model wants run before it continues.
    pub tool_calls: Vec<ToolCall>,
    /// Why the model stopped, in the provider's spelling; empty if not provided.
    pub finish_reason: String,
}

/// Finish reasons meaning the model ran out of room.
pub const TRUNCATED: [&str; 2] = ["length", "max_tokens"];

impl Completion {
    /// Whether the model stopped because it ran out of room.
    #[must_use]
    pub fn was_truncated(&self) -> bool {
        TRUNCATED.contains(&self.finish_reason.as_str())
    }
}

/// Completion backend implemented by a provider extension; fallbacks are tried in order.
pub trait Completer {
    /// Stable id, used in fallback diagnostics.
    fn id(&self) -> &str;

    /// Complete the assembled request.
    ///
    /// # Errors
    /// A human-readable provider failure; the conductor tries the next completer.
    fn complete(&mut self, request: &PendingRequest) -> Result<Completion, String>;
}

/// Result of invoking a tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocation {
    /// The tool's result, or why it failed.
    pub content: String,
    /// Whether the call failed.
    pub failed: bool,
}

/// Routes a tool call to the extension that implements it.
pub trait ToolInvoker {
    /// Invoke `call`; `None` means no tool matched, which the conductor reports as a failed result.
    fn invoke(&mut self, call: &ToolCall) -> Option<ToolInvocation>;

    /// Invoke `call`, letting the tool ask the user via `driver`; defaults to [`Self::invoke`].
    fn invoke_asking(
        &mut self,
        call: &ToolCall,
        _driver: &mut dyn Driver,
    ) -> Option<ToolInvocation> {
        self.invoke(call)
    }

    /// Tells the fleet which session this turn's calls belong to; a no-op by default.
    fn bind_session(&mut self, _session: &str) {}
}

/// A [`ToolInvoker`] with no tools.
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
    /// An interceptor blocked, or a `tool-call` interceptor failed closed.
    Blocked(BlockReason),
}

/// Dispatches phases to a fixed, load-ordered set of interceptors.
pub struct Dispatcher {
    interceptors: Vec<Box<dyn Interceptor>>,
}

impl Dispatcher {
    /// Builds a dispatcher; `interceptors` order is the intra-phase dispatch order.
    #[must_use]
    pub fn new(interceptors: Vec<Box<dyn Interceptor>>) -> Self {
        Self { interceptors }
    }

    /// What the loaded extensions contribute, in load order, skipping empty sets.
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
    /// [`InvokeError::Unknown`] when no loaded extension has that id or contributes that name.
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

    /// Dispatches `phase` to each subscribed interceptor; an `Err` fails closed at [`Phase::ToolCall`], open elsewhere.
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

    #[test]
    fn every_declared_phase_is_one_the_loop_dispatches() {
        for phase in ALL_PHASES {
            assert!(
                is_dispatched(phase),
                "{phase:?} is declared but never dispatched"
            );
        }
    }

    #[test]
    fn the_phase_list_covers_every_variant() {
        let mut seen = ALL_PHASES.to_vec();
        seen.sort_by_key(|p| format!("{p:?}"));
        seen.dedup_by_key(|p| format!("{p:?}"));
        assert_eq!(seen.len(), ALL_PHASES.len(), "no duplicates in ALL_PHASES");
    }
}
