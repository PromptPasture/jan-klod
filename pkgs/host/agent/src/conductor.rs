//! Loop conductor: sequences a turn and runs the `ReAct` loop. Every decision is delegated to interceptors.
//! Decoupled from Wasmtime through [`Completer`] and [`ToolInvoker`], so it is unit-tested with stubs.

use jk_protocol::turn::{Event, EventSink, Flow};

use jk_protocol::intercept::{
    Completer, Completion, Dispatcher, FinalAnswer, HookState, Outcome, PendingRequest, Phase,
    RawResponse, ToolInvocation, ToolInvoker, UserTurn,
};

use jk_protocol::turn::{Driver, Message, Role, ToolCall, ToolOutcome};

/// Default cap on `ReAct` iterations, to stop infinite tool-call loops. Raise via `limits.max-iterations`.
pub const DEFAULT_MAX_ITERATIONS: u32 = 8;

/// Bounds a turn runs under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Cap on `ReAct` cycles for one turn.
    pub max_iterations: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_iterations: DEFAULT_MAX_ITERATIONS,
        }
    }
}

/// Retries for malformed completions before giving up.
const MAX_RETRIES: u32 = 3;

/// An [`EventSink`] that drops every event — the non-streaming default.
pub struct NoSink;
impl EventSink for NoSink {
    fn emit(&mut self, _event: &Event) -> Flow {
        Flow::Continue
    }
}

/// Result of running one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    /// The loop produced an answer.
    Answered {
        /// Final answer text (after `after-response`/`finalize`).
        text: String,
        /// Whether request-shaping path ran.
        agentic: bool,
    },
    /// Turn failed: shaping phase blocked or all fallback providers failed.
    Failed(String),
}

/// Runs one turn through the loop. `providers` is the fallback chain, `tools`
/// routes tool calls, and `driver` answers interceptor `ask`s.
///
/// `on_effective_message` fires once with the message the model receives after
/// `before-loop` rewrites (see [`build_initial_request`]).
#[allow(clippy::too_many_arguments)] // A turn needs all of these; a struct would only hide them.
pub fn run_turn(
    dispatcher: &mut Dispatcher,
    providers: &mut [Box<dyn Completer>],
    tools: &mut dyn ToolInvoker,
    driver: &mut dyn Driver,
    sink: &mut dyn EventSink,
    session: &str,
    user_message: &str,
    history: Vec<Message>,
    limits: Limits,
    principal: Option<String>,
    on_effective_message: &mut dyn FnMut(&str),
) -> RunResult {
    // Before any dispatch: session-scoped tool state needs to know its session.
    tools.bind_session(session);

    let (agentic, mut request) = build_initial_request(
        dispatcher,
        driver,
        session,
        user_message,
        history,
        principal,
        on_effective_message,
    );

    if agentic {
        for phase in [Phase::SelectModel, Phase::SelectContext, Phase::SelectTools] {
            match shape(dispatcher, driver, phase, request) {
                Ok(shaped) => request = shaped,
                Err(reason) => return RunResult::Failed(reason),
            }
        }
    }

    // Every path below sets `final_text`.
    let mut final_text;
    let mut iterations = 0;
    loop {
        let completion = match complete_validated(providers, &request, sink) {
            Ok(completion) => completion,
            Err(reason) => {
                sink.emit(&Event::Warning(reason.clone()));
                return RunResult::Failed(reason);
            }
        };
        let text = post_phase(dispatcher, driver, Phase::AfterResponse, completion.text);
        request.messages.push(Message {
            role: Role::Assistant,
            content: text.clone(),
            tool_call_id: None,
        });
        // The delta is a preview; the terminal `Done` carries the authoritative text.
        let flow = if text.is_empty() {
            Flow::Continue
        } else {
            sink.emit(&Event::TextDelta(text.clone()))
        };
        final_text = text;

        if flow == Flow::Stop {
            // Cancelled mid-thought: record the turn as unfinished, not as an answer.
            final_text = note_incomplete(
                &final_text,
                "stopped before the turn finished — the client disconnected or cancelled",
                sink,
            );
            break;
        }

        let step = if completion.tool_calls.is_empty() {
            handle_no_tool_calls(
                dispatcher,
                driver,
                sink,
                &mut request,
                &mut iterations,
                limits,
                final_text,
            )
        } else {
            iterations += 1;
            if iterations >= limits.max_iterations {
                LoopStep::Break(cut_short(&final_text, limits.max_iterations, sink))
            } else {
                let pass = run_tool_calls(
                    dispatcher,
                    tools,
                    driver,
                    sink,
                    &completion.tool_calls,
                    &mut request,
                );
                after_tool_calls(pass, final_text, sink)
            }
        };
        match step {
            LoopStep::Continue => {}
            LoopStep::Break(text) => {
                final_text = text;
                break;
            }
        }
    }

    let text = post_phase(dispatcher, driver, Phase::Finalize, final_text);
    sink.emit(&Event::Done {
        text: text.clone(),
        agentic,
    });
    RunResult::Answered { text, agentic }
}

/// Assembles the first [`PendingRequest`]: runs `before-loop`, then appends the
/// new message to `history`. Returns whether the agentic path should run.
/// `on_effective_message` fires once, before any loop event (#84).
fn build_initial_request(
    dispatcher: &mut Dispatcher,
    driver: &mut dyn Driver,
    session: &str,
    user_message: &str,
    history: Vec<Message>,
    principal: Option<String>,
    on_effective_message: &mut dyn FnMut(&str),
) -> (bool, PendingRequest) {
    let mut state = HookState::BeforeLoop(UserTurn {
        session: session.to_string(),
        user_message: user_message.to_string(),
        principal,
    });
    let agentic = matches!(
        dispatcher.dispatch(Phase::BeforeLoop, &mut state, driver),
        Outcome::Proceeded
    );
    // The model sees this, which differs from `user_message` after a `replace`.
    let effective_message = match &state {
        HookState::BeforeLoop(turn) => turn.user_message.clone(),
        _ => user_message.to_string(),
    };
    on_effective_message(&effective_message);

    let mut messages = history;
    messages.push(Message {
        role: Role::User,
        content: effective_message,
        tool_call_id: None,
    });
    let request = PendingRequest {
        model: None,
        messages,
        tools: vec![],
        grammar: None,
        max_tokens: None,
        temperature: None,
    };
    (agentic, request)
}

/// What the `ReAct` loop does after a cycle.
enum LoopStep {
    /// Keep looping.
    Continue,
    /// Stop; this text is final.
    Break(String),
}

/// Handle no-tool-call completion: turn ends or driver steers with follow-up.
fn handle_no_tool_calls(
    dispatcher: &mut Dispatcher,
    driver: &mut dyn Driver,
    sink: &mut dyn EventSink,
    request: &mut PendingRequest,
    iterations: &mut u32,
    limits: Limits,
    final_text: String,
) -> LoopStep {
    let Some(follow_up) = driver.follow_up() else {
        return LoopStep::Break(final_text);
    };
    // Optional model/context swap before the follow-up.
    let mut next_state = HookState::PrepareNextTurn(request.clone());
    let _ = dispatcher.dispatch(Phase::PrepareNextTurn, &mut next_state, driver);
    if let HookState::PrepareNextTurn(shaped) = next_state {
        *request = shaped;
    }
    request.messages.push(Message {
        role: Role::User,
        content: follow_up,
        tool_call_id: None,
    });
    *iterations += 1;
    if *iterations >= limits.max_iterations {
        return LoopStep::Break(cut_short(&final_text, limits.max_iterations, sink));
    }
    LoopStep::Continue
}

/// Convert [`ToolPass`] outcome to loop's next step.
fn after_tool_calls(pass: ToolPass, final_text: String, sink: &mut dyn EventSink) -> LoopStep {
    match pass {
        ToolPass::Continue => LoopStep::Continue,
        // Decision: answer in hand is intended.
        ToolPass::Terminated => LoopStep::Break(final_text),
        // Interruption: model mid-thought.
        ToolPass::Cancelled => LoopStep::Break(note_incomplete(
            &final_text,
            "stopped before the turn finished — the client disconnected or cancelled",
            sink,
        )),
    }
}

/// Marks a turn stopped at the cycle cap. The note goes in the text and the
/// events, since headless callers see only the text.
fn cut_short(text: &str, cap: u32, sink: &mut dyn EventSink) -> String {
    note_incomplete(
        text,
        &format!(
            "stopped after {cap} tool cycles — the task was not finished. Raise \
             `limits.max-iterations` if it needs more room."
        ),
        sink,
    )
}

/// Marks a turn ended before the model finished. An unmarked partial answer would teach the model false facts.
fn note_incomplete(text: &str, note: &str, sink: &mut dyn EventSink) -> String {
    sink.emit(&Event::Warning(note.to_string()));
    if text.trim().is_empty() {
        format!("[{note}]")
    } else {
        format!("{text}\n\n[{note}]")
    }
}

/// Why the tool pass ended the loop. Not a bool: a `tool-result` termination is
/// a decision, a sink cancel is an interruption, and merging them would record a disconnect as finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ToolPass {
    /// Keep looping.
    Continue,
    /// Interceptor terminated at `tool-result`.
    Terminated,
    /// Sink cancelled (no listener or explicit stop).
    Cancelled,
}

/// Runs each tool call through the `tool-call` gate, dispatch and `tool-result`,
/// appending each result to the conversation.
fn run_tool_calls(
    dispatcher: &mut Dispatcher,
    tools: &mut dyn ToolInvoker,
    driver: &mut dyn Driver,
    sink: &mut dyn EventSink,
    calls: &[ToolCall],
    request: &mut PendingRequest,
) -> ToolPass {
    let mut stop = ToolPass::Continue;
    for call in calls {
        // Block denies this call; the model is told and the loop continues.
        let mut call_state = HookState::ToolCall(call.clone());
        let invocation = match dispatcher.dispatch(Phase::ToolCall, &mut call_state, driver) {
            Outcome::Blocked(reason) => {
                let _ = sink.emit(&Event::Warning(format!(
                    "tool `{}` denied: {}",
                    call.name, reason.message
                )));
                ToolInvocation {
                    content: format!("tool call denied: {}", reason.message),
                    failed: true,
                }
            }
            Outcome::Proceeded => {
                let effective = match &call_state {
                    HookState::ToolCall(c) => c.clone(),
                    _ => call.clone(),
                };
                if sink.emit(&Event::ToolInvoked(effective.clone())) == Flow::Stop {
                    stop = ToolPass::Cancelled;
                }
                tools
                    .invoke_asking(&effective, driver)
                    .unwrap_or_else(|| ToolInvocation {
                        content: format!("no tool named `{}`", effective.name),
                        failed: true,
                    })
            }
        };

        // A block here terminates the loop.
        let mut result_state = HookState::ToolResult(ToolOutcome {
            tool_call_id: call.id.clone(),
            content: invocation.content,
            failed: invocation.failed,
        });
        if matches!(
            dispatcher.dispatch(Phase::ToolResult, &mut result_state, driver),
            Outcome::Blocked(_)
        ) {
            // Decision, not interruption: answer intended.
            stop = ToolPass::Terminated;
        }
        let outcome = match result_state {
            HookState::ToolResult(outcome) => outcome,
            // An interceptor replaced the state with the wrong case; report failure, not a silent no-op.
            _ => ToolOutcome {
                tool_call_id: call.id.clone(),
                content: String::new(),
                failed: true,
            },
        };
        if sink.emit(&Event::ToolResult(outcome.clone())) == Flow::Stop {
            stop = ToolPass::Cancelled;
        }
        request.messages.push(Message {
            role: Role::Tool,
            content: outcome.content,
            tool_call_id: Some(call.id.clone()),
        });
    }
    stop
}

/// Dispatches a request-shaping phase, returning the (possibly replaced) request.
fn shape(
    dispatcher: &mut Dispatcher,
    driver: &mut dyn Driver,
    phase: Phase,
    request: PendingRequest,
) -> Result<PendingRequest, String> {
    let mut state = match phase {
        Phase::SelectModel => HookState::SelectModel(request),
        Phase::SelectContext => HookState::SelectContext(request),
        Phase::SelectTools => HookState::SelectTools(request),
        _ => return Err(format!("shape called with non-shaping phase {phase:?}")),
    };
    match dispatcher.dispatch(phase, &mut state, driver) {
        Outcome::Blocked(reason) => Err(reason.message),
        Outcome::Proceeded => match state {
            HookState::SelectModel(r) | HookState::SelectContext(r) | HookState::SelectTools(r) => {
                Ok(r)
            }
            _ => Err("interceptor replaced shaping state with the wrong case".to_string()),
        },
    }
}

/// Dispatches `after-response` or `finalize`; an interceptor may rewrite the text via `replace`.
fn post_phase(
    dispatcher: &mut Dispatcher,
    driver: &mut dyn Driver,
    phase: Phase,
    text: String,
) -> String {
    let mut state = match phase {
        Phase::AfterResponse => HookState::AfterResponse(RawResponse {
            text,
            finish_reason: "stop".to_string(),
        }),
        Phase::Finalize => HookState::Finalize(FinalAnswer { text }),
        _ => return text,
    };
    // Blocking has no meaning here; read back whatever state the phase left.
    let _ = dispatcher.dispatch(phase, &mut state, driver);
    match state {
        HookState::AfterResponse(r) => r.text,
        HookState::Finalize(a) => a.text,
        _ => String::new(),
    }
}

/// Try each completer in order; first success wins. On exhaustion, return diagnostic.
fn complete_with_fallback(
    providers: &mut [Box<dyn Completer>],
    request: &PendingRequest,
    sink: &mut dyn EventSink,
) -> Result<Completion, String> {
    if providers.is_empty() {
        return Err("no providers configured".to_string());
    }
    let mut failures = Vec::new();
    for provider in providers.iter_mut() {
        match provider.complete(request) {
            Ok(completion) => return Ok(completion),
            Err(err) => {
                let note = format!("{}: {err}", provider.id());
                sink.emit(&Event::Warning(format!("provider {note}; falling back")));
                failures.push(note);
            }
        }
    }
    Err(format!("all providers failed ({})", failures.join("; ")))
}

/// Completes with a retry harness: malformed output is fed back with a
/// correction and re-issued, up to [`MAX_RETRIES`]. Corrections are transient.
fn complete_validated(
    providers: &mut [Box<dyn Completer>],
    request: &PendingRequest,
    sink: &mut dyn EventSink,
) -> Result<Completion, String> {
    let mut attempt_request = request.clone();
    for attempt in 0..=MAX_RETRIES {
        let completion = complete_with_fallback(providers, &attempt_request, sink)?;
        if completion.was_truncated() {
            // Not an error, but a truncated answer presented as whole is wrong, so warn.
            sink.emit(&Event::Warning(
                "the model stopped at its token limit — this answer is cut off".to_string(),
            ));
        }
        match validate(&completion) {
            Ok(()) => return Ok(completion),
            Err(reason) if attempt == MAX_RETRIES => {
                return Err(format!(
                    "malformed output after {MAX_RETRIES} retries: {reason}"
                ));
            }
            Err(reason) => {
                sink.emit(&Event::Warning(format!(
                    "malformed output; retrying: {reason}"
                )));
                attempt_request.messages.push(Message {
                    role: Role::Assistant,
                    content: completion.text,
                    tool_call_id: None,
                });
                attempt_request.messages.push(Message {
                    role: Role::User,
                    content: format!(
                        "Your previous response was invalid: {reason}. Reply again, correctly."
                    ),
                    tool_call_id: None,
                });
            }
        }
    }
    // Loop returns on last attempt; unreachable.
    Err("retry loop exited unexpectedly".to_string())
}

/// Every tool call's `arguments` must be valid JSON, since the contract encodes them as a string.
fn validate(completion: &Completion) -> Result<(), String> {
    for call in &completion.tool_calls {
        serde_json::from_str::<serde_json::Value>(&call.arguments)
            .map_err(|err| format!("tool `{}` arguments are not valid JSON: {err}", call.name))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use jk_protocol::intercept::{
        BlockReason, Decision, InterceptInput, Interceptor, InterceptorError, NoTools, TRUNCATED,
    };
    use jk_protocol::turn::UserPrompt;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    struct NoDriver;
    impl Driver for NoDriver {
        fn ask(&mut self, _p: &UserPrompt) -> String {
            panic!("no ask expected");
        }
    }

    /// Interceptor returning fixed decision, recording calls.
    struct Stub {
        id: String,
        phases: Vec<Phase>,
        log: Rc<RefCell<Vec<String>>>,
        decision: Decision,
    }
    impl Interceptor for Stub {
        fn id(&self) -> &str {
            &self.id
        }
        fn subscribed_phases(&self) -> Vec<Phase> {
            self.phases.clone()
        }
        fn intercept(&mut self, _input: &InterceptInput) -> Result<Decision, InterceptorError> {
            self.log.borrow_mut().push(self.id.clone());
            Ok(self.decision.clone())
        }
    }

    fn stub(
        id: &str,
        phases: Vec<Phase>,
        log: &Rc<RefCell<Vec<String>>>,
        decision: Decision,
    ) -> Box<Stub> {
        Box::new(Stub {
            id: id.into(),
            phases,
            log: Rc::clone(log),
            decision,
        })
    }

    /// Completer with queue of replies (last repeats when drained).
    struct ScriptedProvider {
        id: String,
        replies: RefCell<VecDeque<Result<Completion, String>>>,
        seen: Rc<RefCell<Vec<Option<String>>>>,
    }
    impl Completer for ScriptedProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn complete(&mut self, request: &PendingRequest) -> Result<Completion, String> {
            self.seen.borrow_mut().push(request.model.clone());
            let mut replies = self.replies.borrow_mut();
            if replies.len() > 1 {
                replies.pop_front().unwrap()
            } else {
                replies.front().cloned().unwrap()
            }
        }
    }

    fn text_provider(id: &str, reply: Result<&str, &str>) -> Box<dyn Completer> {
        let reply = reply
            .map(|t| Completion {
                text: t.into(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
            })
            .map_err(std::string::ToString::to_string);
        Box::new(ScriptedProvider {
            id: id.into(),
            replies: RefCell::new(VecDeque::from(vec![reply])),
            seen: Rc::new(RefCell::new(vec![])),
        })
    }

    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    fn with_tools(text: &str, calls: Vec<ToolCall>) -> Completion {
        Completion {
            text: text.into(),
            tool_calls: calls,
            finish_reason: "stop".into(),
        }
    }

    fn bad_call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "not json".into(),
        }
    }

    fn scripted(id: &str, replies: Vec<Result<Completion, String>>) -> Box<dyn Completer> {
        Box::new(ScriptedProvider {
            id: id.into(),
            replies: RefCell::new(VecDeque::from(replies)),
            seen: Rc::new(RefCell::new(vec![])),
        })
    }

    /// Invoker returning canned result, counting invocations.
    struct CountingTools {
        result: String,
        count: Rc<RefCell<u32>>,
    }
    impl ToolInvoker for CountingTools {
        fn invoke(&mut self, _call: &ToolCall) -> Option<ToolInvocation> {
            *self.count.borrow_mut() += 1;
            Some(ToolInvocation {
                content: self.result.clone(),
                failed: false,
            })
        }
    }

    #[test]
    fn simple_prompt_short_circuits_shaping() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![
            stub(
                "intent",
                vec![Phase::BeforeLoop],
                &log,
                Decision::Block(BlockReason {
                    message: "simple".into(),
                }),
            ),
            stub("model", vec![Phase::SelectModel], &log, Decision::Proceed),
        ]);
        let mut providers = vec![text_provider("p", Ok("hi there"))];
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "hello",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert_eq!(
            out,
            RunResult::Answered {
                text: "hi there".into(),
                agentic: false
            }
        );
        assert_eq!(
            *log.borrow(),
            vec!["intent"],
            "shaping must not run on simple path"
        );
    }

    #[test]
    fn agentic_prompt_runs_shaping_then_completes() {
        let log = Rc::new(RefCell::new(vec![]));
        let seen = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![
            stub("intent", vec![Phase::BeforeLoop], &log, Decision::Proceed),
            stub(
                "model",
                vec![Phase::SelectModel],
                &log,
                Decision::Replace(HookState::SelectModel(PendingRequest {
                    model: Some("gpt-x".into()),
                    messages: vec![],
                    tools: vec![],
                    grammar: None,
                    max_tokens: None,
                    temperature: None,
                })),
            ),
        ]);
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![Ok(Completion {
                text: "done".into(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
            })])),
            seen: Rc::clone(&seen),
        })];
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "do many things",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert_eq!(
            out,
            RunResult::Answered {
                text: "done".into(),
                agentic: true
            }
        );
        assert_eq!(*log.borrow(), vec!["intent", "model"]);
        assert_eq!(*seen.borrow(), vec![Some("gpt-x".to_string())]);
    }

    /// #84: `on_effective_message` must see the `before-loop` rewrite, not the original.
    #[test]
    fn on_effective_message_sees_a_before_loop_rewrite_not_the_original() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![stub(
            "rewrite",
            vec![Phase::BeforeLoop],
            &log,
            Decision::Replace(HookState::BeforeLoop(UserTurn {
                session: "s".into(),
                user_message: "rewritten".into(),
                principal: None,
            })),
        )]);
        let mut providers = vec![text_provider("p", Ok("hi"))];
        let seen = Rc::new(RefCell::new(Vec::<String>::new()));
        let mut record = |effective: &str| seen.borrow_mut().push(effective.to_string());
        run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "original",
            vec![],
            Limits::default(),
            None,
            &mut record,
        );

        assert_eq!(
            *seen.borrow(),
            vec!["rewritten".to_string()],
            "the hook must fire exactly once, with the rewrite, not `original`"
        );
    }

    /// A turn hitting the cycle cap says so in both text and stream.
    #[test]
    fn hitting_the_cycle_cap_is_reported_not_hidden() {
        let mut d = Dispatcher::new(vec![]);
        // Model never stops asking for tools.
        let forever = Completion {
            text: String::new(),
            tool_calls: vec![ToolCall {
                id: "1".into(),
                name: "fs".into(),
                arguments: "{}".into(),
            }],
            finish_reason: "tool_calls".into(),
        };
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "loop".into(),
            replies: RefCell::new(VecDeque::from(vec![Ok(forever)])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = CountingTools {
            result: "ok".into(),
            count: Rc::new(RefCell::new(0)),
        };
        let mut sink = RecordingSink(vec![]);
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut tools,
            &mut NoDriver,
            &mut sink,
            "s",
            "keep going",
            vec![],
            Limits { max_iterations: 3 },
            None,
            &mut |_: &str| {},
        );

        let RunResult::Answered { text, .. } = out else {
            panic!("the turn still answers")
        };
        assert!(
            text.contains("stopped after 3 tool cycles"),
            "the answer says it was cut short: {text:?}"
        );
        assert!(
            text.contains("limits.max-iterations"),
            "and names the knob that raises it: {text:?}"
        );
        let warned = sink.0.iter().any(
            |event| matches!(event, Event::Warning(message) if message.contains("stopped after 3")),
        );
        assert!(warned, "a streaming client hears it too: {:?}", sink.0);
    }

    #[test]
    fn react_loop_runs_two_cycles_then_answers() {
        let mut d = Dispatcher::new(vec![]);
        let count = Rc::new(RefCell::new(0));
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![
                Ok(with_tools("", vec![call("1", "search")])),
                Ok(with_tools("", vec![call("2", "fetch")])),
                Ok(with_tools("final answer", vec![])),
            ])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = CountingTools {
            result: "ok".into(),
            count: Rc::clone(&count),
        };
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut tools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "multi-step",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert_eq!(
            out,
            RunResult::Answered {
                text: "final answer".into(),
                agentic: true
            }
        );
        assert_eq!(
            *count.borrow(),
            2,
            "two tool calls invoked across two ReAct cycles"
        );
    }

    #[test]
    fn permission_block_denies_the_call_but_loop_continues() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![stub(
            "perm",
            vec![Phase::ToolCall],
            &log,
            Decision::Block(BlockReason {
                message: "denied".into(),
            }),
        )]);
        let count = Rc::new(RefCell::new(0));
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![
                Ok(with_tools("", vec![call("1", "rm")])),
                Ok(with_tools("done anyway", vec![])),
            ])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = CountingTools {
            result: "ok".into(),
            count: Rc::clone(&count),
        };
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut tools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "please rm",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert_eq!(
            out,
            RunResult::Answered {
                text: "done anyway".into(),
                agentic: true
            }
        );
        assert_eq!(*count.borrow(), 0, "a denied tool call is never invoked");
    }

    #[test]
    fn tool_result_block_terminates_the_loop() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![stub(
            "term",
            vec![Phase::ToolResult],
            &log,
            Decision::Block(BlockReason {
                message: "stop".into(),
            }),
        )]);
        // Provider keeps emitting tool calls; terminate stops it.
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![Ok(with_tools(
                "partial",
                vec![call("1", "loop")],
            ))])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = NoTools;
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut tools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "go",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        // Exactly text, no "stopped before" note: terminate is decision, not interruption.
        assert_eq!(
            out,
            RunResult::Answered {
                text: "partial".into(),
                agentic: true
            }
        );
    }

    #[test]
    fn malformed_output_triggers_retry_with_correction() {
        let mut d = Dispatcher::new(vec![]);
        // First completion has invalid JSON args; retry returns clean text.
        let mut providers = vec![scripted(
            "p",
            vec![
                Ok(with_tools("", vec![bad_call("1", "search")])),
                Ok(with_tools("recovered", vec![])),
            ],
        )];
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "go",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert_eq!(
            out,
            RunResult::Answered {
                text: "recovered".into(),
                agentic: true
            }
        );
    }

    #[test]
    fn persistently_malformed_output_fails_after_retries() {
        let mut d = Dispatcher::new(vec![]);
        // Single reply repeating: always malformed → give up after retries.
        let mut providers = vec![scripted(
            "p",
            vec![Ok(with_tools("", vec![bad_call("1", "x")]))],
        )];
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "go",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert!(
            matches!(out, RunResult::Failed(msg) if msg.contains("malformed output after 3 retries"))
        );
    }

    #[test]
    fn provider_fallback_uses_the_next_on_failure() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![
            text_provider("primary", Err("rate-limited")),
            text_provider("backup", Ok("recovered")),
        ];
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "hi",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert_eq!(
            out,
            RunResult::Answered {
                text: "recovered".into(),
                agentic: true
            }
        );
    }

    #[test]
    fn exhausted_fallback_chain_fails() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![
            text_provider("primary", Err("rate-limited")),
            text_provider("backup", Err("transient")),
        ];
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "hi",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert!(matches!(out, RunResult::Failed(msg) if msg.contains("all providers failed")));
    }

    #[test]
    fn finalize_can_rewrite_the_authoritative_text() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![stub(
            "final",
            vec![Phase::Finalize],
            &log,
            Decision::Replace(HookState::Finalize(FinalAnswer {
                text: "redacted".into(),
            })),
        )]);
        let mut providers = vec![text_provider("p", Ok("secret"))];
        // No before-loop interceptor → default Proceeded → agentic path.
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut NoSink,
            "s",
            "hello",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert_eq!(
            out,
            RunResult::Answered {
                text: "redacted".into(),
                agentic: true
            }
        );
    }

    /// Sink recording every emitted event.
    #[derive(Default)]
    struct RecordingSink(Vec<Event>);
    impl EventSink for RecordingSink {
        fn emit(&mut self, event: &Event) -> Flow {
            self.0.push(event.clone());
            Flow::Continue
        }
    }

    /// Sink recording events, returning `Stop` after `after` events to test cancellation.
    struct CancelAfter {
        events: Vec<Event>,
        after: usize,
    }
    impl EventSink for CancelAfter {
        fn emit(&mut self, event: &Event) -> Flow {
            self.events.push(event.clone());
            if self.events.len() >= self.after {
                Flow::Stop
            } else {
                Flow::Continue
            }
        }
    }

    #[test]
    fn simple_turn_streams_delta_then_done() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![text_provider("p", Ok("hi there"))];
        let mut sink = RecordingSink::default();
        run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut sink,
            "s",
            "hello",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert_eq!(
            sink.0,
            vec![
                Event::TextDelta("hi there".into()),
                Event::Done {
                    text: "hi there".into(),
                    agentic: true
                },
            ]
        );
    }

    #[test]
    fn react_turn_streams_tool_events_between_deltas() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![
                Ok(with_tools("", vec![call("1", "search")])),
                Ok(with_tools("final answer", vec![])),
            ])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = CountingTools {
            result: "hit".into(),
            count: Rc::new(RefCell::new(0)),
        };
        let mut sink = RecordingSink::default();
        run_turn(
            &mut d,
            &mut providers,
            &mut tools,
            &mut NoDriver,
            &mut sink,
            "s",
            "go",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        // First completion has no text (only tool call), so no leading delta.
        assert_eq!(
            sink.0,
            vec![
                Event::ToolInvoked(call("1", "search")),
                Event::ToolResult(ToolOutcome {
                    tool_call_id: "1".into(),
                    content: "hit".into(),
                    failed: false
                }),
                Event::TextDelta("final answer".into()),
                Event::Done {
                    text: "final answer".into(),
                    agentic: true
                },
            ]
        );
    }

    /// Driver injecting one follow-up, then stops steering.
    struct SteeringDriver {
        follow_ups: std::cell::RefCell<Vec<&'static str>>,
    }
    impl Driver for SteeringDriver {
        fn ask(&mut self, _p: &UserPrompt) -> String {
            panic!("no ask expected");
        }
        fn follow_up(&mut self) -> Option<String> {
            self.follow_ups.borrow_mut().pop().map(str::to_string)
        }
    }

    #[test]
    fn a_follow_up_injects_another_cycle() {
        let mut d = Dispatcher::new(vec![]);
        // The driver steers with a follow-up after the first text completion.
        let seen = Rc::new(RefCell::new(vec![]));
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![
                Ok(with_tools("first", vec![])),
                Ok(with_tools("second", vec![])),
            ])),
            seen: Rc::clone(&seen),
        })];
        let mut driver = SteeringDriver {
            follow_ups: std::cell::RefCell::new(vec!["and now this"]),
        };
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut driver,
            &mut NoSink,
            "s",
            "go",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert_eq!(
            out,
            RunResult::Answered {
                text: "second".into(),
                agentic: true
            }
        );
        assert_eq!(
            seen.borrow().len(),
            2,
            "the follow-up drove a second completion"
        );
    }

    #[test]
    fn sink_stop_cancels_the_react_loop() {
        // Provider keeps emitting; sink stopping after first result must cancel at boundary.
        let mut d = Dispatcher::new(vec![]);
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![Ok(with_tools(
                "",
                vec![call("1", "loop")],
            ))])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = CountingTools {
            result: "r".into(),
            count: Rc::new(RefCell::new(0)),
        };
        let mut sink = CancelAfter {
            events: vec![],
            after: 2,
        };
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut tools,
            &mut NoDriver,
            &mut sink,
            "s",
            "go",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert!(
            matches!(out, RunResult::Answered { .. }),
            "a cancelled turn still finalizes"
        );
        assert!(
            matches!(sink.events.last(), Some(Event::Done { .. })),
            "the loop stopped and emitted a terminal Done: {:?}",
            sink.events
        );
        // Recorded as unfinished, not as real answer.
        let RunResult::Answered { text, .. } = out else {
            unreachable!()
        };
        assert!(
            text.contains("stopped before the turn finished"),
            "the cancelled turn is marked, not passed off as an answer: {text:?}"
        );
    }

    #[test]
    fn fallback_streams_a_warning() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![
            text_provider("primary", Err("rate-limited")),
            text_provider("backup", Ok("recovered")),
        ];
        let mut sink = RecordingSink::default();
        run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut sink,
            "s",
            "hi",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert!(
            matches!(&sink.0[0], Event::Warning(w) if w.contains("primary") && w.contains("falling back")),
            "first event should be a fallback warning: {:?}",
            sink.0
        );
        assert!(matches!(sink.0.last(), Some(Event::Done { .. })));
    }

    /// Provider whose completion stopped at token limit.
    fn truncated_provider() -> Box<dyn Completer> {
        Box::new(ScriptedProvider {
            id: "cut".into(),
            replies: RefCell::new(VecDeque::from(vec![Ok(Completion {
                text: "the first half of the ans".into(),
                tool_calls: vec![],
                finish_reason: "length".into(),
            })])),
            seen: Rc::new(RefCell::new(vec![])),
        })
    }

    #[test]
    fn a_truncated_answer_says_so_instead_of_just_stopping() {
        // The text is real, but presenting it as finished would be wrong.
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![truncated_provider()];
        let mut sink = RecordingSink::default();
        let out = run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut sink,
            "s",
            "hi",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert!(
            sink.0
                .iter()
                .any(|e| matches!(e, Event::Warning(w) if w.contains("cut off"))),
            "the truncation is surfaced: {:?}",
            sink.0
        );
        // Still an answer, not failure: partial text is best available.
        assert!(
            matches!(&out, RunResult::Answered { text, .. } if text.contains("first half")),
            "the partial text is still returned: {out:?}"
        );
    }

    #[test]
    fn an_ordinary_completion_warns_about_nothing() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![text_provider("p", Ok("all of it"))];
        let mut sink = RecordingSink::default();
        run_turn(
            &mut d,
            &mut providers,
            &mut NoTools,
            &mut NoDriver,
            &mut sink,
            "s",
            "hi",
            vec![],
            Limits::default(),
            None,
            &mut |_: &str| {},
        );

        assert!(
            !sink.0.iter().any(|e| matches!(e, Event::Warning(_))),
            "a finished answer is not flagged: {:?}",
            sink.0
        );
    }

    #[test]
    fn both_spellings_of_the_limit_count_as_truncated() {
        // `length` emitted by first-party guests (provider-anthropic maps `max_tokens`);
        // raw spelling accepted too.
        for reason in TRUNCATED {
            let completion = Completion {
                text: "x".into(),
                tool_calls: vec![],
                finish_reason: (*reason).to_string(),
            };
            assert!(completion.was_truncated(), "`{reason}` is a truncation");
        }
        for reason in ["stop", "tool_calls", "tool-calls", ""] {
            let completion = Completion {
                text: "x".into(),
                tool_calls: vec![],
                finish_reason: reason.to_string(),
            };
            assert!(
                !completion.was_truncated(),
                "`{reason}` is not a truncation"
            );
        }
    }
}
