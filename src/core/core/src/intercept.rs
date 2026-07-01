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
    /// Once per session, before any turn.
    SessionStart,
    /// Per run, before the agentic loop; may short-circuit to a direct answer.
    BeforeLoop,
    /// Request-shaping: pick the model first.
    SelectModel,
    /// Request-shaping: trim/compress history to the model's budget.
    SelectContext,
    /// Request-shaping: fix the tool set.
    SelectTools,
    /// Non-recoverable error — observation and graceful degradation only.
    OnError,
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
#[derive(Debug, Clone)]
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

/// Session-scoped context handed at `session-start`.
#[derive(Debug, Clone)]
pub struct SessionCtx {
    /// Opaque session identifier.
    pub session: String,
}

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
#[derive(Debug, Clone)]
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

/// State handed at `on-error` — observation only in v1.
#[derive(Debug, Clone)]
pub struct ErrorInfo {
    /// Which phase the error occurred in or after.
    pub failed_phase: Phase,
    /// Human-readable error description.
    pub message: String,
}

/// Phase-specific state handed to an interceptor. The active case always matches
/// the phase being dispatched (mirrors `interceptor.hook-state`).
#[derive(Debug, Clone)]
pub enum HookState {
    /// `session-start` payload.
    SessionStart(SessionCtx),
    /// `before-loop` payload.
    BeforeLoop(UserTurn),
    /// `select-model` payload.
    SelectModel(PendingRequest),
    /// `select-context` payload.
    SelectContext(PendingRequest),
    /// `select-tools` payload.
    SelectTools(PendingRequest),
    /// `on-error` payload.
    OnError(ErrorInfo),
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

/// The attached driver (TUI, chat, api-*) that answers an `Ask`. In Phase 2 this
/// is the offline test harness.
pub trait Driver {
    /// Surface `prompt` and return the user's answer (or a default when headless).
    fn ask(&mut self, prompt: &UserPrompt) -> String;
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
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    /// A configurable stub: records each call into a shared log, then returns a
    /// canned decision (or, for the ask case, `Ask` until an answer arrives).
    struct Stub {
        id: String,
        phases: Vec<Phase>,
        log: Rc<RefCell<Vec<String>>>,
        behavior: Behavior,
    }

    #[derive(Clone)]
    enum Behavior {
        Proceed,
        Block,
        Err(InterceptorError),
        ReplaceUserMessage(String),
        AskThenProceed(String),
    }

    impl Interceptor for Stub {
        fn id(&self) -> &str {
            &self.id
        }
        fn subscribed_phases(&self) -> Vec<Phase> {
            self.phases.clone()
        }
        fn intercept(&mut self, input: &InterceptInput) -> Result<Decision, InterceptorError> {
            self.log.borrow_mut().push(self.id.clone());
            match &self.behavior {
                Behavior::Proceed => Ok(Decision::Proceed),
                Behavior::Block => Ok(Decision::Block(BlockReason {
                    message: format!("{} blocked", self.id),
                })),
                Behavior::Err(e) => Err(*e),
                Behavior::ReplaceUserMessage(text) => {
                    Ok(Decision::Replace(HookState::BeforeLoop(UserTurn {
                        session: "s".into(),
                        user_message: text.clone(),
                    })))
                }
                Behavior::AskThenProceed(expected) => input.answer.as_ref().map_or_else(
                    || {
                        Ok(Decision::Ask(UserPrompt {
                            question: "ok?".into(),
                            options: vec![],
                            default_answer: String::new(),
                        }))
                    },
                    |answer| {
                        assert_eq!(answer, expected, "interceptor must see the driver's answer");
                        Ok(Decision::Proceed)
                    },
                ),
            }
        }
    }

    struct CannedDriver {
        answer: String,
        asked: Rc<RefCell<u32>>,
    }

    impl Driver for CannedDriver {
        fn ask(&mut self, _prompt: &UserPrompt) -> String {
            *self.asked.borrow_mut() += 1;
            self.answer.clone()
        }
    }

    fn no_driver() -> CannedDriver {
        CannedDriver {
            answer: String::new(),
            asked: Rc::new(RefCell::new(0)),
        }
    }

    fn stub(
        id: &str,
        phases: Vec<Phase>,
        log: &Rc<RefCell<Vec<String>>>,
        behavior: Behavior,
    ) -> Box<dyn Interceptor> {
        Box::new(Stub {
            id: id.into(),
            phases,
            log: Rc::clone(log),
            behavior,
        })
    }

    #[test]
    fn dispatch_order_is_across_and_within_phase() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut d = Dispatcher::new(vec![
            stub("a", vec![Phase::BeforeLoop], &log, Behavior::Proceed),
            stub("b", vec![Phase::BeforeLoop], &log, Behavior::Proceed),
            stub("c", vec![Phase::SelectModel], &log, Behavior::Proceed),
        ]);
        let mut driver = no_driver();

        let mut before = HookState::BeforeLoop(UserTurn {
            session: "s".into(),
            user_message: "hi".into(),
        });
        assert!(matches!(d.dispatch(Phase::BeforeLoop, &mut before, &mut driver), Outcome::Proceeded));
        // c must not fire on before-loop.
        assert_eq!(*log.borrow(), vec!["a", "b"]);

        let mut sm = HookState::SelectModel(PendingRequest {
            model: None,
            messages: vec![],
            tools: vec![],
            grammar: None,
            max_tokens: None,
            temperature: None,
        });
        assert!(matches!(d.dispatch(Phase::SelectModel, &mut sm, &mut driver), Outcome::Proceeded));
        assert_eq!(*log.borrow(), vec!["a", "b", "c"]);
    }

    #[test]
    fn replace_swaps_the_phase_state() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut d = Dispatcher::new(vec![stub(
            "rw",
            vec![Phase::BeforeLoop],
            &log,
            Behavior::ReplaceUserMessage("rewritten".into()),
        )]);
        let mut driver = no_driver();
        let mut state = HookState::BeforeLoop(UserTurn {
            session: "s".into(),
            user_message: "original".into(),
        });
        d.dispatch(Phase::BeforeLoop, &mut state, &mut driver);
        match state {
            HookState::BeforeLoop(turn) => assert_eq!(turn.user_message, "rewritten"),
            _ => panic!("state case changed"),
        }
    }

    #[test]
    fn block_short_circuits_remaining_interceptors() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut d = Dispatcher::new(vec![
            stub("first", vec![Phase::BeforeLoop], &log, Behavior::Block),
            stub("second", vec![Phase::BeforeLoop], &log, Behavior::Proceed),
        ]);
        let mut driver = no_driver();
        let mut state = HookState::BeforeLoop(UserTurn {
            session: "s".into(),
            user_message: "hi".into(),
        });
        assert!(matches!(d.dispatch(Phase::BeforeLoop, &mut state, &mut driver), Outcome::Blocked(_)));
        assert_eq!(*log.borrow(), vec!["first"], "second must not run after a block");
    }

    #[test]
    fn tool_call_error_fails_closed() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut d = Dispatcher::new(vec![stub(
            "gate",
            vec![Phase::ToolCall],
            &log,
            Behavior::Err(InterceptorError::Internal),
        )]);
        let mut driver = no_driver();
        let mut state = HookState::ToolCall(ToolCall {
            id: "1".into(),
            name: "rm".into(),
            arguments: "{}".into(),
        });
        assert!(
            matches!(d.dispatch(Phase::ToolCall, &mut state, &mut driver), Outcome::Blocked(_)),
            "a failing tool-call interceptor must fail closed"
        );
    }

    #[test]
    fn non_tool_call_error_fails_open() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let mut d = Dispatcher::new(vec![
            stub("boom", vec![Phase::BeforeLoop], &log, Behavior::Err(InterceptorError::Internal)),
            stub("after", vec![Phase::BeforeLoop], &log, Behavior::Proceed),
        ]);
        let mut driver = no_driver();
        let mut state = HookState::BeforeLoop(UserTurn {
            session: "s".into(),
            user_message: "hi".into(),
        });
        assert!(matches!(d.dispatch(Phase::BeforeLoop, &mut state, &mut driver), Outcome::Proceeded));
        // The faulty interceptor did not wedge the loop; the next one still ran.
        assert_eq!(*log.borrow(), vec!["boom", "after"]);
    }

    #[test]
    fn ask_round_trips_through_the_driver() {
        let log = Rc::new(RefCell::new(Vec::new()));
        let asked = Rc::new(RefCell::new(0));
        let mut d = Dispatcher::new(vec![stub(
            "asker",
            vec![Phase::ToolCall],
            &log,
            Behavior::AskThenProceed("yes".into()),
        )]);
        let mut driver = CannedDriver {
            answer: "yes".into(),
            asked: Rc::clone(&asked),
        };
        let mut state = HookState::ToolCall(ToolCall {
            id: "1".into(),
            name: "rm".into(),
            arguments: "{}".into(),
        });
        assert!(matches!(d.dispatch(Phase::ToolCall, &mut state, &mut driver), Outcome::Proceeded));
        assert_eq!(*asked.borrow(), 1, "driver asked exactly once");
        // asker invoked twice: once returning Ask, once with the answer.
        assert_eq!(*log.borrow(), vec!["asker", "asker"]);
    }
}
