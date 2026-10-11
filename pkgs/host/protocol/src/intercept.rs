//! The interceptor contract and the two backends a loop calls out to.
//!
//! The host-side mirror of `wit/interceptor.wit` — [`Phase`], [`HookState`],
//! [`Decision`], the [`Interceptor`] trait — plus [`Completer`] and
//! [`ToolInvoker`], the seams a turn reaches a model and a tool through.
//! Plain types with no Component-Model dependency: the wasm guest adapters
//! implement these traits, the loop that drives them consumes them, and
//! neither has to name the other ([decisions/2026-07-01-thin-loop-interceptors]).
//!
//! They live here, not beside the loop, because the adapters and the loop are
//! separate crates and the traits are the only thing both must agree on (#181).
//! Keeping a trait with its consumer would make the adapters depend on the loop
//! and the loop on the adapters it composes.
//!
//! [`Dispatcher`] is the phase-agnostic engine over that contract: at each
//! [`Phase`] it calls every subscribed interceptor in load order and applies the
//! fail-closed / fail-open error policy. It holds no policy and no Wasmtime, so
//! it sits with the contract — the adapters' tests drive it as the loop does.
//! Ordering is structural, never config-driven: *across* phases the loop calls
//! [`Dispatcher::dispatch`] once per phase in [`Phase`] declaration order;
//! *within* one phase, the load order interceptors were registered in.

use crate::contributions::{ArgumentValue, Contributions, InvokeError, InvokeOutcome};
use crate::turn::{Driver, Message, ToolCall, ToolOutcome, UserPrompt};

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
/// The match is **exhaustive on purpose**: adding a [`Phase`] variant won't
/// compile until someone says here whether the loop reaches it — otherwise an
/// extension could silently subscribe to a phase that never runs.
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

/// One interceptor the engine can drive. A wasm guest adapter owns a `Store` +
/// generated bindings and implements this by calling the guest's exports; tests
/// implement it with plain stubs.
pub trait Interceptor {
    /// Stable id, used in logs and fail-closed reasons.
    fn id(&self) -> &str;

    /// Which phases this interceptor wants dispatched to it.
    fn subscribed_phases(&self) -> Vec<Phase>;

    /// Decide, for one dispatch.
    ///
    /// # Errors
    /// Returns [`InterceptorError`] when the interceptor itself malfunctions
    /// (not to be confused with [`Decision::Block`], a normal outcome). The
    /// engine applies the fail-closed-at-`tool-call` policy.
    fn intercept(&mut self, input: &InterceptInput) -> Result<Decision, InterceptorError>;

    /// What this extension contributes to a client's interface, if anything.
    ///
    /// Defaulted to nothing, because contributing is opt-in at every level:
    /// most interceptors do not export `client-surface`, and a host that
    /// required every implementer to say "none" would make the absence of a
    /// declaration look like an omission rather than the answer it is.
    fn contributions(&mut self) -> Option<crate::contributions::Contributions> {
        None
    }

    /// Run a contribution this extension declared.
    ///
    /// # Errors
    /// [`crate::contributions::InvokeError::Unknown`] when nothing of that name
    /// is contributed — which is also the default, since an extension that
    /// declares nothing can run nothing.
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
    /// Why the model stopped (provider spelling: `stop`, `length`, `tool_calls`, …).
    /// Empty if not provided. Kept (not discarded) because `length` means the answer
    /// is cut off mid-thought—a truncated answer that looks finished is actionable wrong.
    /// See [`TRUNCATED`].
    pub finish_reason: String,
}

/// Finish-reasons indicating model ran out of room, not finished.
/// `length` is OpenAI-compatible spelling; raw `max_tokens` also accepted.
pub const TRUNCATED: [&str; 2] = ["length", "max_tokens"];

impl Completion {
    /// Whether the model stopped because it ran out of room.
    #[must_use]
    pub fn was_truncated(&self) -> bool {
        TRUNCATED.contains(&self.finish_reason.as_str())
    }
}

/// Completion backend; implemented by provider extension. Fallback chain tries in order.
pub trait Completer {
    /// Stable id, used in fallback diagnostics.
    fn id(&self) -> &str;

    /// Complete the assembled request.
    ///
    /// # Errors
    /// Returns a human-readable provider failure. The conductor treats any error
    /// as a fallback trigger and tries the next completer in the chain.
    fn complete(&mut self, request: &PendingRequest) -> Result<Completion, String>;
}

/// Result of invoking a tool: content and whether it failed.
/// Previously (#162), no way to distinguish failure except reading prose meant for humans.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInvocation {
    /// The tool's result, or a human-readable description of why it failed.
    pub content: String,
    /// Whether the call failed.
    pub failed: bool,
}

/// A tool backend: routes a tool call to the extension that implements it.
pub trait ToolInvoker {
    /// Invoke `call`. `None` means no tool matched (skip-if-absent);
    /// conductor reports as failed result, doesn't abort the turn.
    fn invoke(&mut self, call: &ToolCall) -> Option<ToolInvocation>;

    /// Invoke `call`, letting the tool ask the user first if it can (#216).
    ///
    /// Default: ignore the driver and behave like [`Self::invoke`]. Only a
    /// fleet holding components that export `tool-askable` has anything to
    /// do with it, and the default means adding this broke no
    /// implementation.
    ///
    /// The driver is **passed, not stored**, which is the whole reason this
    /// is a separate method rather than state like
    /// [`Self::bind_session`]. A fleet that held a driver across a turn
    /// would be holding a second mutable borrow of it, and a tool would be
    /// able to ask outside a tool call.
    fn invoke_asking(
        &mut self,
        call: &ToolCall,
        _driver: &mut dyn Driver,
    ) -> Option<ToolInvocation> {
        self.invoke(call)
    }

    /// Tell the fleet which session the calls in this turn belong to.
    ///
    /// Default: ignore it. Only a fleet whose guests keep session-scoped
    /// state needs this, and a no-op default means adding it broke no
    /// implementation (#215).
    ///
    /// Called once per turn by the loop, which is the only place a tool
    /// is dispatched from. A fleet that needs a session and is not given one
    /// **refuses** rather than falling back to shared state, so forgetting
    /// this is an error rather than a silent leak between sessions.
    fn bind_session(&mut self, _session: &str) {}
}

/// A [`ToolInvoker`] with no tools — every call is absent. Used for turns that
/// offer no tools (e.g. the simple/inline path).
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
    /// Build a dispatcher from interceptors in load (registration) order — the
    /// sole source of intra-phase ordering.
    #[must_use]
    pub fn new(interceptors: Vec<Box<dyn Interceptor>>) -> Self {
        Self { interceptors }
    }

    /// What the loaded extensions contribute to a client's interface, in load
    /// order, skipping those that contribute nothing.
    ///
    /// Read on demand rather than cached at boot: an extension may report a
    /// change after an invocation, and a set read once would then be stale in
    /// every client at the same time.
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
