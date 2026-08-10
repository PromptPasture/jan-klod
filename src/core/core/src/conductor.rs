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

/// Default cap on `ReAct` iterations, so a model that keeps emitting tool calls
/// can never spin forever — and on a metered endpoint, never spend forever.
///
/// Eight is a real constraint for coding work: view, edit, run the tests, read the
/// failure, fix, run again is already six. Raise it with `limits.max-iterations`
/// when a task needs the room and you are watching the bill.
pub const DEFAULT_MAX_ITERATIONS: u32 = 8;

/// Bounds a turn runs under.
///
/// A struct rather than another parameter because the next one to arrive
/// (`max-retries`, a wall-clock budget) belongs beside this rather than widening
/// every signature between here and `build_agent` again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Cap on `ReAct` cycles for one turn.
    pub max_iterations: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Self { max_iterations: DEFAULT_MAX_ITERATIONS }
    }
}

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
    /// Why the model stopped, verbatim from the provider (`stop`, `length`,
    /// `tool_calls`, …). Empty when the provider did not say.
    ///
    /// Carried rather than discarded because `length` means the answer is **cut
    /// off mid-thought**, and a truncated answer that looks like a finished one is
    /// the kind of wrong a user acts on. See [`TRUNCATED`].
    pub finish_reason: String,
}

/// The `finish-reason`s that mean the model ran out of room rather than finishing.
///
/// `length` is the OpenAI-compatible spelling, and what `provider-anthropic`
/// normalises its `max_tokens` to. The raw `max_tokens` is accepted as well, since
/// a third-party guest may pass the provider's own wording through rather than
/// mapping it — a signal this specific should not be lost to a spelling.
pub const TRUNCATED: [&str; 2] = ["length", "max_tokens"];

impl Completion {
    /// Whether the model stopped because it ran out of room.
    #[must_use]
    pub fn was_truncated(&self) -> bool {
        TRUNCATED.contains(&self.finish_reason.as_str())
    }
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

/// An incremental event emitted as a turn runs.
///
/// Streamed to a driver (a live TUI transcript, SSE over the REST surface) via an
/// [`EventSink`]. `text-delta`s are a non-authoritative **preview**; the terminal
/// `Done` carries the authoritative answer (which `after-response`/`finalize` may
/// have rewritten).
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

/// Whether the loop should keep running after an event.
///
/// A sink returns [`Flow::Stop`] to **cancel** the turn at the next loop boundary —
/// e.g. an SSE sink whose client disconnected, or an explicit stop button.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    /// Keep going.
    Continue,
    /// Cancel the turn at the next boundary (finalize with what's in hand).
    Stop,
}

/// A sink the conductor pushes [`Event`]s to as a turn runs — synchronously, on the
/// turn's own thread (the loop is sync and the session is `!Send`). Returning
/// [`Flow::Stop`] cancels the turn.
pub trait EventSink {
    /// Handle one event; return [`Flow::Stop`] to cancel the turn.
    fn emit(&mut self, event: &Event) -> Flow;
}

/// An [`EventSink`] that drops every event — the non-streaming default.
pub struct NoSink;
impl EventSink for NoSink {
    fn emit(&mut self, _event: &Event) -> Flow {
        Flow::Continue
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
#[allow(clippy::too_many_arguments)] // A turn genuinely needs all of these; a
// bag-of-fields struct would only move the list somewhere less visible.
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

    // Prior turns first, then this one. Without this the model saw a single
    // message per turn and a session had no memory at all: "now add a test for
    // that" reached a model that had never seen "that". Trimming the result to
    // the model's window is `select-context`'s job, which is why it now has
    // something to trim.
    let mut messages = history;
    messages.push(Message {
        role: Role::User,
        content: effective_message,
        tool_call_id: None,
    });
    let mut request = PendingRequest {
        model: None,
        messages,
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
        let completion = match complete_validated(providers, &request, sink) {
            Ok(completion) => completion,
            Err(reason) => {
                sink.emit(&Event::Warning(reason.clone()));
                return RunResult::Failed(reason);
            }
        };
        // after-response sees the raw output and may rewrite the text.
        let text = post_phase(dispatcher, driver, Phase::AfterResponse, completion.text);
        request.messages.push(Message {
            role: Role::Assistant,
            content: text.clone(),
            tool_call_id: None,
        });
        // Preview delta; the authoritative text is in the terminal Done. A sink
        // returning Stop (e.g. client disconnected) cancels the turn here.
        let flow = if text.is_empty() {
            Flow::Continue
        } else {
            sink.emit(&Event::TextDelta(text.clone()))
        };
        final_text = text;

        if flow == Flow::Stop {
            break;
        }
        if completion.tool_calls.is_empty() {
            // The turn would end. A driver may steer it with a follow-up message;
            // otherwise finish.
            let Some(follow_up) = driver.follow_up() else { break };
            // prepare-next-turn: optional model/context swap before continuing.
            let mut next_state = HookState::PrepareNextTurn(request.clone());
            let _ = dispatcher.dispatch(Phase::PrepareNextTurn, &mut next_state, driver);
            if let HookState::PrepareNextTurn(shaped) = next_state {
                request = shaped;
            }
            request.messages.push(Message {
                role: Role::User,
                content: follow_up,
                tool_call_id: None,
            });
            iterations += 1;
            if iterations >= limits.max_iterations {
                final_text = cut_short(final_text, limits.max_iterations, sink);
                break;
            }
            continue;
        }
        iterations += 1;
        if iterations >= limits.max_iterations {
            final_text = cut_short(final_text, limits.max_iterations, sink);
            break;
        }

        // Returns true to stop the loop: a tool-result `terminate`, or a sink cancel.
        if run_tool_calls(dispatcher, tools, driver, sink, &completion.tool_calls, &mut request) {
            break;
        }
    }

    let text = post_phase(dispatcher, driver, Phase::Finalize, final_text);
    sink.emit(&Event::Done { text: text.clone(), agentic });
    RunResult::Answered { text, agentic }
}

/// Note that the turn stopped at its cycle cap rather than because it was done.
///
/// The cap used to `break` silently, so a task needing more steps than the limit
/// returned whatever the last completion happened to say — often a fragment, and
/// when the model was mid-tool-call, nothing at all — presented as the answer. The
/// truncation warning exists for the same reason: a half-finished answer that looks
/// finished is a wrong the reader acts on. The note goes in the text as well as on
/// the event stream, because a headless caller (`ask`, a CI step) sees only text.
fn cut_short(text: String, cap: u32, sink: &mut dyn EventSink) -> String {
    let note = format!(
        "stopped after {cap} tool cycles — the task was not finished. Raise \
         `limits.max-iterations` if it needs more room."
    );
    sink.emit(&Event::Warning(note.clone()));
    if text.trim().is_empty() {
        format!("[{note}]")
    } else {
        format!("{text}\n\n[{note}]")
    }
}

/// Run each tool call: gate at `tool-call`, dispatch the tool, run `tool-result`,
/// and append the result to the conversation. Returns `true` if a `tool-result`
/// interceptor blocked — the loop's `terminate` signal.
fn run_tool_calls(
    dispatcher: &mut Dispatcher,
    tools: &mut dyn ToolInvoker,
    driver: &mut dyn Driver,
    sink: &mut dyn EventSink,
    calls: &[ToolCall],
    request: &mut PendingRequest,
) -> bool {
    let mut stop = false;
    for call in calls {
        // tool-call gate (e.g. permission). A block denies just this call; the
        // model is told, and the loop continues.
        let mut call_state = HookState::ToolCall(call.clone());
        let content = match dispatcher.dispatch(Phase::ToolCall, &mut call_state, driver) {
            Outcome::Blocked(reason) => {
                let _ = sink.emit(&Event::Warning(format!("tool `{}` denied: {}", call.name, reason.message)));
                format!("tool call denied: {}", reason.message)
            }
            Outcome::Proceeded => {
                let effective = match &call_state {
                    HookState::ToolCall(c) => c.clone(),
                    _ => call.clone(),
                };
                if sink.emit(&Event::ToolInvoked(effective.clone())) == Flow::Stop {
                    stop = true;
                }
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
            stop = true; // tool-result `terminate`
        }
        let outcome = match result_state {
            HookState::ToolResult(outcome) => outcome,
            _ => ToolOutcome { tool_call_id: call.id.clone(), content: String::new() },
        };
        if sink.emit(&Event::ToolResult(outcome.clone())) == Flow::Stop {
            stop = true; // sink cancel
        }
        request.messages.push(Message {
            role: Role::Tool,
            content: outcome.content,
            tool_call_id: Some(call.id.clone()),
        });
    }
    stop
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

/// Complete with the small-model harness: on a malformed completion, feed the bad
/// output back with a correction and re-issue, up to [`MAX_RETRIES`] times. The
/// correction context is transient (a local copy of the request) so it never
/// pollutes the real conversation; only a valid completion is returned.
fn complete_validated(
    providers: &mut [Box<dyn Completer>],
    request: &PendingRequest,
    sink: &mut dyn EventSink,
) -> Result<Completion, String> {
    let mut attempt_request = request.clone();
    for attempt in 0..=MAX_RETRIES {
        let completion = complete_with_fallback(providers, &attempt_request, sink)?;
        if completion.was_truncated() {
            // Not an error: the text so far is real and worth returning. But a
            // truncated answer presented as a whole one is a wrong the user acts
            // on, so say it rather than letting the sentence just stop.
            sink.emit(&Event::Warning(
                "the model stopped at its token limit — this answer is cut off".to_string(),
            ));
        }
        match validate(&completion) {
            Ok(()) => return Ok(completion),
            Err(reason) if attempt == MAX_RETRIES => {
                return Err(format!("malformed output after {MAX_RETRIES} retries: {reason}"));
            }
            Err(reason) => {
                sink.emit(&Event::Warning(format!("malformed output; retrying: {reason}")));
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
        Completion { text: text.into(), tool_calls: calls, finish_reason: "stop".into() }
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
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut NoSink, "s", "hello", vec![], Limits::default());
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
            replies: RefCell::new(VecDeque::from(vec![Ok(Completion {
                text: "done".into(),
                tool_calls: vec![],
                finish_reason: "stop".into(),
            })])),
            seen: Rc::clone(&seen),
        })];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut NoSink, "s", "do many things", vec![], Limits::default());
        assert_eq!(out, RunResult::Answered { text: "done".into(), agentic: true });
        assert_eq!(*log.borrow(), vec!["intent", "model"]);
        assert_eq!(*seen.borrow(), vec![Some("gpt-x".to_string())]);
    }

    /// A turn that hits the cycle cap says so, in the text and on the stream.
    ///
    /// The cap used to `break` silently, so a task needing more steps returned
    /// whatever the last completion happened to say — often a fragment, sometimes
    /// nothing — presented as the answer. Same reasoning as the truncation warning:
    /// a half-finished answer that looks finished is a wrong the reader acts on.
    #[test]
    fn hitting_the_cycle_cap_is_reported_not_hidden() {
        let mut d = Dispatcher::new(vec![]);
        // A model that never stops asking for tools.
        let forever = Completion {
            text: String::new(),
            tool_calls: vec![ToolCall { id: "1".into(), name: "fs".into(), arguments: "{}".into() }],
            finish_reason: "tool_calls".into(),
        };
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "loop".into(),
            replies: RefCell::new(VecDeque::from(vec![Ok(forever)])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = CountingTools { result: "ok".into(), count: Rc::new(RefCell::new(0)) };
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
        );

        let RunResult::Answered { text, .. } = out else { panic!("the turn still answers") };
        assert!(
            text.contains("stopped after 3 tool cycles"),
            "the answer says it was cut short: {text:?}"
        );
        assert!(
            text.contains("limits.max-iterations"),
            "and names the knob that raises it: {text:?}"
        );
        let warned = sink.0.iter().any(|event| {
            matches!(event, Event::Warning(message) if message.contains("stopped after 3"))
        });
        assert!(warned, "a streaming client hears it too: {:?}", sink.0);
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
        let out = run_turn(&mut d, &mut providers, &mut tools, &mut NoDriver, &mut NoSink, "s", "multi-step", vec![], Limits::default());
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
        let out = run_turn(&mut d, &mut providers, &mut tools, &mut NoDriver, &mut NoSink, "s", "please rm", vec![], Limits::default());
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
        let out = run_turn(&mut d, &mut providers, &mut tools, &mut NoDriver, &mut NoSink, "s", "go", vec![], Limits::default());
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
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut NoSink, "s", "go", vec![], Limits::default());
        assert_eq!(out, RunResult::Answered { text: "recovered".into(), agentic: true });
    }

    #[test]
    fn persistently_malformed_output_fails_after_retries() {
        let mut d = Dispatcher::new(vec![]);
        // A single reply that repeats: always malformed -> give up after retries.
        let mut providers = vec![scripted("p", vec![Ok(with_tools("", vec![bad_call("1", "x")]))])];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut NoSink, "s", "go", vec![], Limits::default());
        assert!(matches!(out, RunResult::Failed(msg) if msg.contains("malformed output after 3 retries")));
    }

    #[test]
    fn provider_fallback_uses_the_next_on_failure() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![
            text_provider("primary", Err("rate-limited")),
            text_provider("backup", Ok("recovered")),
        ];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut NoSink, "s", "hi", vec![], Limits::default());
        assert_eq!(out, RunResult::Answered { text: "recovered".into(), agentic: true });
    }

    #[test]
    fn exhausted_fallback_chain_fails() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![
            text_provider("primary", Err("rate-limited")),
            text_provider("backup", Err("transient")),
        ];
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut NoSink, "s", "hi", vec![], Limits::default());
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
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut NoSink, "s", "hello", vec![], Limits::default());
        assert_eq!(out, RunResult::Answered { text: "redacted".into(), agentic: true });
    }

    /// A sink that records every emitted event.
    #[derive(Default)]
    struct RecordingSink(Vec<Event>);
    impl EventSink for RecordingSink {
        fn emit(&mut self, event: &Event) -> Flow {
            self.0.push(event.clone());
            Flow::Continue
        }
    }

    /// A sink that records events and returns `Stop` once it has seen `after` of
    /// them — to test cancellation at a loop boundary.
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
        run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut sink, "s", "hello", vec![], Limits::default());
        assert_eq!(
            sink.0,
            vec![
                Event::TextDelta("hi there".into()),
                Event::Done { text: "hi there".into(), agentic: true },
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
        let mut tools = CountingTools { result: "hit".into(), count: Rc::new(RefCell::new(0)) };
        let mut sink = RecordingSink::default();
        run_turn(&mut d, &mut providers, &mut tools, &mut NoDriver, &mut sink, "s", "go", vec![], Limits::default());
        // First completion had no text (only a tool call), so no leading delta.
        assert_eq!(
            sink.0,
            vec![
                Event::ToolInvoked(call("1", "search")),
                Event::ToolResult(ToolOutcome { tool_call_id: "1".into(), content: "hit".into() }),
                Event::TextDelta("final answer".into()),
                Event::Done { text: "final answer".into(), agentic: true },
            ]
        );
    }

    /// A driver that injects one follow-up message, then stops steering.
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
        // Two text completions (no tools): the first would end the turn, but the
        // driver steers with a follow-up, producing a second completion.
        let seen = Rc::new(RefCell::new(vec![]));
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![
                Ok(with_tools("first", vec![])),
                Ok(with_tools("second", vec![])),
            ])),
            seen: Rc::clone(&seen),
        })];
        let mut driver = SteeringDriver { follow_ups: std::cell::RefCell::new(vec!["and now this"]) };
        let out = run_turn(&mut d, &mut providers, &mut NoTools, &mut driver, &mut NoSink, "s", "go", vec![], Limits::default());
        assert_eq!(out, RunResult::Answered { text: "second".into(), agentic: true });
        assert_eq!(seen.borrow().len(), 2, "the follow-up drove a second completion");
    }

    #[test]
    fn sink_stop_cancels_the_react_loop() {
        // The provider would keep emitting tool calls forever; a sink that stops
        // after the first tool result must cancel the loop at that boundary.
        let mut d = Dispatcher::new(vec![]);
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(ScriptedProvider {
            id: "p".into(),
            replies: RefCell::new(VecDeque::from(vec![Ok(with_tools("", vec![call("1", "loop")]))])),
            seen: Rc::new(RefCell::new(vec![])),
        })];
        let mut tools = CountingTools { result: "r".into(), count: Rc::new(RefCell::new(0)) };
        // Events: ToolInvoked, ToolResult (stop here), then Done at finalize.
        let mut sink = CancelAfter { events: vec![], after: 2 };
        let out = run_turn(&mut d, &mut providers, &mut tools, &mut NoDriver, &mut sink, "s", "go", vec![], Limits::default());
        assert!(matches!(out, RunResult::Answered { .. }), "a cancelled turn still finalizes");
        assert!(
            matches!(sink.events.last(), Some(Event::Done { .. })),
            "the loop stopped and emitted a terminal Done: {:?}",
            sink.events
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
        run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut sink, "s", "hi", vec![], Limits::default());
        assert!(
            matches!(&sink.0[0], Event::Warning(w) if w.contains("primary") && w.contains("falling back")),
            "first event should be a fallback warning: {:?}",
            sink.0
        );
        assert!(matches!(sink.0.last(), Some(Event::Done { .. })));
    }

    /// A provider whose single completion stopped at the token limit.
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
        // The text is real and worth returning — but presented as a finished
        // answer it is the kind of wrong a user acts on.
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![truncated_provider()];
        let mut sink = RecordingSink::default();
        let out =
            run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut sink, "s", "hi", vec![], Limits::default());

        assert!(
            sink.0.iter().any(|e| matches!(e, Event::Warning(w) if w.contains("cut off"))),
            "the truncation is surfaced: {:?}",
            sink.0
        );
        // Still an answer, not a failure: the partial text is the best available.
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
        run_turn(&mut d, &mut providers, &mut NoTools, &mut NoDriver, &mut sink, "s", "hi", vec![], Limits::default());
        assert!(
            !sink.0.iter().any(|e| matches!(e, Event::Warning(_))),
            "a finished answer is not flagged: {:?}",
            sink.0
        );
    }

    #[test]
    fn both_spellings_of_the_limit_count_as_truncated() {
        // `length` is what both first-party guests emit (provider-anthropic maps
        // Anthropic's `max_tokens` onto it); the raw spelling is accepted too.
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
            assert!(!completion.was_truncated(), "`{reason}` is not a truncation");
        }
    }
}
