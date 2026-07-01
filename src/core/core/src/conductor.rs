//! The thin loop conductor — core mechanism, zero policy.
//!
//! Where [`crate::intercept::Dispatcher`] runs *one* phase, the conductor
//! sequences a turn: `before-loop` (which may short-circuit a simple prompt), the
//! request-shaping phases (`select-model` → `select-context` → `select-tools`),
//! then the **`ReAct` loop** — `complete()` (with provider fallback) →
//! `after-response` → parse tool calls → `tool-call` gate → tool dispatch →
//! `tool-result` (a block here terminates the loop) → repeat — and finally
//! `finalize`. Every decision is delegated to interceptors; the conductor only
//! holds mechanism.
//!
//! Like the dispatcher, it is decoupled from Wasmtime: completions go through the
//! [`Completer`] trait and tools through the [`ToolInvoker`] trait, so the state
//! machine is unit-tested with stubs. The wasm wiring (routed provider as a
//! `Completer`, a `run-handle` streaming entry, and retry-with-correction) lands
//! in following Slice 2c increments.

use crate::intercept::{
    Dispatcher, Driver, FinalAnswer, HookState, Message, Outcome, PendingRequest, Phase,
    RawResponse, Role, ToolCall, ToolOutcome, UserTurn,
};

/// Hard cap on `ReAct` iterations, so a model that keeps emitting tool calls can
/// never spin forever. Tunable via `host-config` later; a safe default for now.
const MAX_ITERATIONS: u32 = 8;

/// How many times a malformed completion is re-issued with a correction before
/// the turn gives up (no silent spiral). Default per the plan; `host-config`
/// override lands with the wasm run entry.
const MAX_RETRIES: u32 = 3;

/// One model completion: the assistant text plus any tool calls it emitted.
#[derive(Debug, Clone, Default)]
pub struct Completion {
    /// Assistant text (may be empty when the model only emitted tool calls).
    pub text: String,
    /// Tool calls the model wants run before it continues.
    pub tool_calls: Vec<ToolCall>,
}

/// A completion backend the conductor can call. Implemented by the routed
/// provider extension; a fallback chain is a list of these tried in order.
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

/// A tool backend: routes a tool call to the extension that implements it.
pub trait ToolInvoker {
    /// Invoke `call`, returning its result content. `None` means no tool matched
    /// (skip-if-absent) — the conductor feeds that back to the model as an error
    /// result rather than aborting the turn.
    fn invoke(&mut self, call: &ToolCall) -> Option<String>;
}

/// A [`ToolInvoker`] with no tools — every call is absent. Used for turns that
/// offer no tools (e.g. the simple/inline path).
pub struct NoTools;
impl ToolInvoker for NoTools {
    fn invoke(&mut self, _call: &ToolCall) -> Option<String> {
        None
    }
}

/// The result of running one turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunResult {
    /// The loop produced an answer. `agentic` is `false` when `before-loop`
    /// short-circuited (intent=simple) and `true` when the shaping phases ran.
    Answered {
        /// The final answer text (after `after-response`/`finalize`).
        text: String,
        /// Whether the agentic (request-shaping) path ran.
        agentic: bool,
    },
    /// The turn could not complete: a shaping phase blocked, or every provider in
    /// the fallback chain failed.
    Failed(String),
}

/// Run one turn through the loop.
///
/// `providers` is the fallback chain (tried in order on failure). `tools` routes
/// tool calls. `driver` answers any interceptor `ask`. The conductor never
/// inspects the payloads it threads — all policy lives in the dispatched
/// interceptors.
pub fn run_turn(
    dispatcher: &mut Dispatcher,
    providers: &mut [Box<dyn Completer>],
    tools: &mut dyn ToolInvoker,
    driver: &mut dyn Driver,
    session: &str,
    user_message: &str,
) -> RunResult {
    // before-loop: may short-circuit a simple prompt.
    let mut state = HookState::BeforeLoop(UserTurn {
        session: session.to_string(),
        user_message: user_message.to_string(),
    });
    let agentic = matches!(
        dispatcher.dispatch(Phase::BeforeLoop, &mut state, driver),
        Outcome::Proceeded
    );
    // An interceptor may have rewritten the user message via `replace`.
    let effective_message = match &state {
        HookState::BeforeLoop(turn) => turn.user_message.clone(),
        _ => user_message.to_string(),
    };

    let mut request = PendingRequest {
        model: None,
        messages: vec![Message {
            role: Role::User,
            content: effective_message,
            tool_call_id: None,
        }],
        tools: vec![],
        grammar: None,
        max_tokens: None,
        temperature: None,
    };

    // Agentic path shapes the request; the simple path answers inline as-is.
    if agentic {
        for phase in [Phase::SelectModel, Phase::SelectContext, Phase::SelectTools] {
            match shape(dispatcher, driver, phase, request) {
                Ok(shaped) => request = shaped,
                Err(reason) => return RunResult::Failed(reason),
            }
        }
    }

    // The ReAct loop: complete → (tool calls?) → run tools → repeat. Every path
    // out of the loop assigns `final_text` first (or returns early).
    let mut final_text;
    let mut iterations = 0;
    loop {
        let completion = match complete_validated(providers, &request) {
            Ok(completion) => completion,
            Err(reason) => return RunResult::Failed(reason),
        };
        // after-response sees the raw output and may rewrite the text.
        let text = post_phase(dispatcher, driver, Phase::AfterResponse, completion.text);
        request.messages.push(Message {
            role: Role::Assistant,
            content: text.clone(),
            tool_call_id: None,
        });
        final_text = text;

        if completion.tool_calls.is_empty() {
            break;
        }
        iterations += 1;
        if iterations >= MAX_ITERATIONS {
            break;
        }

        if run_tool_calls(dispatcher, tools, driver, &completion.tool_calls, &mut request) {
            break; // a tool-result interceptor terminated the loop
        }
    }

    let text = post_phase(dispatcher, driver, Phase::Finalize, final_text);
    RunResult::Answered { text, agentic }
}

/// Run each tool call: gate at `tool-call`, dispatch the tool, run `tool-result`,
/// and append the result to the conversation. Returns `true` if a `tool-result`
/// interceptor blocked — the loop's `terminate` signal.
fn run_tool_calls(
    dispatcher: &mut Dispatcher,
    tools: &mut dyn ToolInvoker,
    driver: &mut dyn Driver,
    calls: &[ToolCall],
    request: &mut PendingRequest,
) -> bool {
    let mut terminate = false;
    for call in calls {
        // tool-call gate (e.g. permission). A block denies just this call; the
        // model is told, and the loop continues.
        let mut call_state = HookState::ToolCall(call.clone());
        let content = match dispatcher.dispatch(Phase::ToolCall, &mut call_state, driver) {
            Outcome::Blocked(reason) => format!("tool call denied: {}", reason.message),
            Outcome::Proceeded => {
                let effective = match &call_state {
                    HookState::ToolCall(c) => c.clone(),
                    _ => call.clone(),
                };
                tools
                    .invoke(&effective)
                    .unwrap_or_else(|| format!("no tool named `{}`", effective.name))
            }
        };

        // tool-result: may rewrite the result; a block terminates the loop.
        let mut result_state = HookState::ToolResult(ToolOutcome {
            tool_call_id: call.id.clone(),
            content,
        });
        if matches!(
            dispatcher.dispatch(Phase::ToolResult, &mut result_state, driver),
            Outcome::Blocked(_)
        ) {
            terminate = true;
        }
        let content = match result_state {
            HookState::ToolResult(outcome) => outcome.content,
            _ => String::new(),
        };
        request.messages.push(Message {
            role: Role::Tool,
            content,
            tool_call_id: Some(call.id.clone()),
        });
    }
    terminate
}

/// Dispatch a request-shaping phase, threading the `PendingRequest` through the
/// matching `hook-state` case and returning the (possibly replaced) request.
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
            HookState::SelectModel(r)
            | HookState::SelectContext(r)
            | HookState::SelectTools(r) => Ok(r),
            _ => Err("interceptor replaced shaping state with the wrong case".to_string()),
        },
    }
}

/// Dispatch a post-completion phase (`after-response` / `finalize`), letting an
/// interceptor rewrite the authoritative text via `replace`.
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
    // A block at these phases has no short-circuit meaning; ignore the outcome and
    // read back whatever text the phase left in place.
    let _ = dispatcher.dispatch(phase, &mut state, driver);
    match state {
        HookState::AfterResponse(r) => r.text,
        HookState::Finalize(a) => a.text,
        _ => String::new(),
    }
}

/// Try each completer in order; the first success wins. On exhaustion, return a
/// diagnostic naming the chain.
fn complete_with_fallback(
    providers: &mut [Box<dyn Completer>],
    request: &PendingRequest,
) -> Result<Completion, String> {
    if providers.is_empty() {
        return Err("no providers configured".to_string());
    }
    let mut failures = Vec::new();
    for provider in providers.iter_mut() {
        match provider.complete(request) {
            Ok(completion) => return Ok(completion),
            Err(err) => failures.push(format!("{}: {err}", provider.id())),
        }
    }
    Err(format!("all providers failed ({})", failures.join("; ")))
}

/// Complete with the small-model harness: on a malformed completion, feed the bad
/// output back with a correction and re-issue, up to [`MAX_RETRIES`] times. The
/// correction context is transient (a local copy of the request) so it never
/// pollutes the real conversation; only a valid completion is returned.
fn complete_validated(
    providers: &mut [Box<dyn Completer>],
    request: &PendingRequest,
) -> Result<Completion, String> {
    let mut attempt_request = request.clone();
    for attempt in 0..=MAX_RETRIES {
        let completion = complete_with_fallback(providers, &attempt_request)?;
        match validate(&completion) {
            Ok(()) => return Ok(completion),
            Err(reason) if attempt == MAX_RETRIES => {
                return Err(format!("malformed output after {MAX_RETRIES} retries: {reason}"));
            }
            Err(reason) => {
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
    // The loop returns on the last attempt; this is unreachable.
    Err("retry loop exited unexpectedly".to_string())
}

/// Structural validation of a completion: every tool call's `arguments` must be
/// valid JSON (the `tool-callable` contract encodes arguments as a JSON string).
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
    use crate::intercept::{
        BlockReason, Decision, InterceptInput, Interceptor, InterceptorError, UserPrompt,
    };
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    struct NoDriver;
    impl Driver for NoDriver {
        fn ask(&mut self, _p: &UserPrompt) -> String {
            panic!("no ask expected");
        }
    }

    /// Interceptor that returns a fixed decision at its phases and records calls.
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

    fn stub(id: &str, phases: Vec<Phase>, log: &Rc<RefCell<Vec<String>>>, decision: Decision) -> Box<Stub> {
        Box::new(Stub {
            id: id.into(),
            phases,
            log: Rc::clone(log),
            decision,
        })
    }

    /// Completer scripted with a queue of replies (last repeats when drained).
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
            .map(|t| Completion { text: t.into(), tool_calls: vec![] })
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
        Completion { text: text.into(), tool_calls: calls }
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

    /// Invoker that returns a canned result for any call and counts invocations.
    struct CountingTools {
        result: String,
        count: Rc<RefCell<u32>>,
    }
    impl ToolInvoker for CountingTools {
        fn invoke(&mut self, _call: &ToolCall) -> Option<String> {
            *self.count.borrow_mut() += 1;
            Some(self.result.clone())
        }
    }

    #[test]
    fn simple_prompt_short_circuits_shaping() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![
            stub("intent", vec![Phase::BeforeLoop], &log, Decision::Block(BlockReason { message: "simple".into() })),
            stub("model", vec![Phase::SelectModel], &log, Decision::Proceed),
        ]);
        let mut providers = vec![text_provider("p", Ok("hi there"))];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, "s", "hello");
        assert_eq!(out, RunResult::Answered { text: "hi there".into(), agentic: false });
        assert_eq!(*log.borrow(), vec!["intent"], "shaping must not run on the simple path");
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
            replies: RefCell::new(VecDeque::from(vec![Ok(Completion { text: "done".into(), tool_calls: vec![] })])),
            seen: Rc::clone(&seen),
        })];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, "s", "do many things");
        assert_eq!(out, RunResult::Answered { text: "done".into(), agentic: true });
        assert_eq!(*log.borrow(), vec!["intent", "model"]);
        assert_eq!(*seen.borrow(), vec![Some("gpt-x".to_string())]);
    }

    #[test]
    fn react_loop_runs_two_cycles_then_answers() {
        let mut d = Dispatcher::new(vec![]);
        let count = Rc::new(RefCell::new(0));
        // Two tool-calling completions, then a final text answer.
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![
                Ok(with_tools("", vec![call("1", "search")])),
                Ok(with_tools("", vec![call("2", "fetch")])),
                Ok(with_tools("final answer", vec![])),
            ])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = CountingTools { result: "ok".into(), count: Rc::clone(&count) };
        let out = run_turn(&mut d, &mut providers, &mut tools, &mut NoDriver, "s", "multi-step");
        assert_eq!(out, RunResult::Answered { text: "final answer".into(), agentic: true });
        assert_eq!(*count.borrow(), 2, "two tool calls invoked across two ReAct cycles");
    }

    #[test]
    fn permission_block_denies_the_call_but_loop_continues() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![stub(
            "perm",
            vec![Phase::ToolCall],
            &log,
            Decision::Block(BlockReason { message: "denied".into() }),
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
        let mut tools = CountingTools { result: "ok".into(), count: Rc::clone(&count) };
        let out = run_turn(&mut d, &mut providers, &mut tools, &mut NoDriver, "s", "please rm");
        assert_eq!(out, RunResult::Answered { text: "done anyway".into(), agentic: true });
        assert_eq!(*count.borrow(), 0, "a denied tool call is never invoked");
    }

    #[test]
    fn tool_result_block_terminates_the_loop() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![stub(
            "term",
            vec![Phase::ToolResult],
            &log,
            Decision::Block(BlockReason { message: "stop".into() }),
        )]);
        // Provider would keep emitting tool calls forever; the terminate stops it.
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![Ok(with_tools("partial", vec![call("1", "loop")]))])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = NoTools;
        let out = run_turn(&mut d, &mut providers, &mut tools, &mut NoDriver, "s", "go");
        assert_eq!(out, RunResult::Answered { text: "partial".into(), agentic: true });
    }

    #[test]
    fn malformed_output_triggers_retry_with_correction() {
        let mut d = Dispatcher::new(vec![]);
        // First completion has a tool call with invalid JSON args; the retry
        // returns clean text.
        let mut providers = vec![scripted(
            "p",
            vec![
                Ok(with_tools("", vec![bad_call("1", "search")])),
                Ok(with_tools("recovered", vec![])),
            ],
        )];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, "s", "go");
        assert_eq!(out, RunResult::Answered { text: "recovered".into(), agentic: true });
    }

    #[test]
    fn persistently_malformed_output_fails_after_retries() {
        let mut d = Dispatcher::new(vec![]);
        // A single reply that repeats: always malformed -> give up after retries.
        let mut providers = vec![scripted("p", vec![Ok(with_tools("", vec![bad_call("1", "x")]))])];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, "s", "go");
        assert!(matches!(out, RunResult::Failed(msg) if msg.contains("malformed output after 3 retries")));
    }

    #[test]
    fn provider_fallback_uses_the_next_on_failure() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![
            text_provider("primary", Err("rate-limited")),
            text_provider("backup", Ok("recovered")),
        ];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, "s", "hi");
        assert_eq!(out, RunResult::Answered { text: "recovered".into(), agentic: true });
    }

    #[test]
    fn exhausted_fallback_chain_fails() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![
            text_provider("primary", Err("rate-limited")),
            text_provider("backup", Err("transient")),
        ];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, "s", "hi");
        assert!(matches!(out, RunResult::Failed(msg) if msg.contains("all providers failed")));
    }

    #[test]
    fn finalize_can_rewrite_the_authoritative_text() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![stub(
            "final",
            vec![Phase::Finalize],
            &log,
            Decision::Replace(HookState::Finalize(FinalAnswer { text: "redacted".into() })),
        )]);
        let mut providers = vec![text_provider("p", Ok("secret"))];
        // No before-loop interceptor -> default Proceeded -> agentic path.
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, "s", "hello");
        assert_eq!(out, RunResult::Answered { text: "redacted".into(), agentic: true });
    }
}
