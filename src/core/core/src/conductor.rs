//! The thin loop conductor — core mechanism, zero policy.
//!
//! Where [`crate::intercept::Dispatcher`] runs *one* phase, the conductor
//! sequences the phases of a turn: `before-loop` (which may short-circuit a
//! simple prompt), the request-shaping phases (`select-model` → `select-context`
//! → `select-tools`), a provider completion with fallback, then `after-response`
//! and `finalize`. Every decision is delegated to interceptors; the conductor
//! only holds mechanism.
//!
//! Like the dispatcher, it is decoupled from Wasmtime: completions go through the
//! [`Completer`] trait, so the state machine is unit-tested with stubs. The wasm
//! wiring (routed provider as a `Completer`, a `run-handle` streaming entry, the
//! `ReAct` tool loop, and retry-with-correction) lands in following Slice 2c
//! increments; this is the phase skeleton + provider fallback.

use crate::intercept::{
    Dispatcher, Driver, FinalAnswer, HookState, Message, Outcome, PendingRequest, Phase,
    RawResponse, Role, UserTurn,
};

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
    fn complete(&mut self, request: &PendingRequest) -> Result<String, String>;
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
/// `providers` is the fallback chain (tried in order on failure). `driver`
/// answers any interceptor `ask`. The conductor never inspects the payloads it
/// threads — all policy lives in the dispatched interceptors.
pub fn run_turn(
    dispatcher: &mut Dispatcher,
    providers: &mut [Box<dyn Completer>],
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

    // Complete with provider fallback.
    let text = match complete_with_fallback(providers, &request) {
        Ok(text) => text,
        Err(reason) => return RunResult::Failed(reason),
    };

    // after-response then finalize — either may rewrite the authoritative text.
    let text = post_phase(dispatcher, driver, Phase::AfterResponse, text);
    let text = post_phase(dispatcher, driver, Phase::Finalize, text);

    RunResult::Answered { text, agentic }
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
) -> Result<String, String> {
    if providers.is_empty() {
        return Err("no providers configured".to_string());
    }
    let mut failures = Vec::new();
    for provider in providers.iter_mut() {
        match provider.complete(request) {
            Ok(text) => return Ok(text),
            Err(err) => failures.push(format!("{}: {err}", provider.id())),
        }
    }
    Err(format!("all providers failed ({})", failures.join("; ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intercept::{
        BlockReason, Decision, InterceptInput, Interceptor, InterceptorError, UserPrompt,
    };
    use std::cell::RefCell;
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

    /// Completer that records the requests it saw and returns a canned reply or a
    /// failure.
    struct StubProvider {
        id: String,
        reply: Result<String, String>,
        seen: Rc<RefCell<Vec<Option<String>>>>,
    }
    impl Completer for StubProvider {
        fn id(&self) -> &str {
            &self.id
        }
        fn complete(&mut self, request: &PendingRequest) -> Result<String, String> {
            self.seen.borrow_mut().push(request.model.clone());
            self.reply.clone()
        }
    }

    fn provider(id: &str, reply: Result<String, String>) -> Box<dyn Completer> {
        Box::new(StubProvider {
            id: id.into(),
            reply,
            seen: Rc::new(RefCell::new(vec![])),
        })
    }

    #[test]
    fn simple_prompt_short_circuits_shaping() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![
            Box::new(Stub {
                id: "intent".into(),
                phases: vec![Phase::BeforeLoop],
                log: Rc::clone(&log),
                decision: Decision::Block(BlockReason { message: "simple".into() }),
            }),
            Box::new(Stub {
                id: "model".into(),
                phases: vec![Phase::SelectModel],
                log: Rc::clone(&log),
                decision: Decision::Proceed,
            }),
        ]);
        let mut providers = vec![provider("p", Ok("hi there".into()))];
        let out = run_turn(&mut d, &mut providers, &mut NoDriver, "s", "hello");
        assert_eq!(out, RunResult::Answered { text: "hi there".into(), agentic: false });
        // The shaping interceptor must not have run.
        assert_eq!(*log.borrow(), vec!["intent"]);
    }

    #[test]
    fn agentic_prompt_runs_shaping_then_completes() {
        let log = Rc::new(RefCell::new(vec![]));
        let seen = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![
            Box::new(Stub {
                id: "intent".into(),
                phases: vec![Phase::BeforeLoop],
                log: Rc::clone(&log),
                decision: Decision::Proceed,
            }),
            Box::new(Stub {
                id: "model".into(),
                phases: vec![Phase::SelectModel],
                log: Rc::clone(&log),
                // Set the model on the shaped request.
                decision: Decision::Replace(HookState::SelectModel(PendingRequest {
                    model: Some("gpt-x".into()),
                    messages: vec![],
                    tools: vec![],
                    grammar: None,
                    max_tokens: None,
                    temperature: None,
                })),
            }),
        ]);
        let mut providers: Vec<Box<dyn Completer>> = vec![Box::new(StubProvider {
            id: "p".into(),
            reply: Ok("done".into()),
            seen: Rc::clone(&seen),
        })];
        let out = run_turn(&mut d, &mut providers, &mut NoDriver, "s", "do many things");
        assert_eq!(out, RunResult::Answered { text: "done".into(), agentic: true });
        assert_eq!(*log.borrow(), vec!["intent", "model"]);
        // The completer saw the model the shaping phase set.
        assert_eq!(*seen.borrow(), vec![Some("gpt-x".to_string())]);
    }

    #[test]
    fn provider_fallback_uses_the_next_on_failure() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![
            provider("primary", Err("rate-limited".into())),
            provider("backup", Ok("recovered".into())),
        ];
        let out = run_turn(&mut d, &mut providers, &mut NoDriver, "s", "hi");
        assert_eq!(out, RunResult::Answered { text: "recovered".into(), agentic: true });
    }

    #[test]
    fn exhausted_fallback_chain_fails() {
        let mut d = Dispatcher::new(vec![]);
        let mut providers = vec![
            provider("primary", Err("rate-limited".into())),
            provider("backup", Err("transient".into())),
        ];
        let out = run_turn(&mut d, &mut providers, &mut NoDriver, "s", "hi");
        assert!(matches!(out, RunResult::Failed(msg) if msg.contains("all providers failed")));
    }

    #[test]
    fn finalize_can_rewrite_the_authoritative_text() {
        let log = Rc::new(RefCell::new(vec![]));
        let mut d = Dispatcher::new(vec![Box::new(Stub {
            id: "final".into(),
            phases: vec![Phase::Finalize],
            log,
            decision: Decision::Replace(HookState::Finalize(FinalAnswer {
                text: "redacted".into(),
            })),
        })]);
        let mut providers = vec![provider("p", Ok("secret".into()))];
        // No before-loop interceptor -> default Proceeded -> agentic path.
        let out = run_turn(&mut d, &mut providers, &mut NoDriver, "s", "hello");
        assert_eq!(out, RunResult::Answered { text: "redacted".into(), agentic: true });
    }
}
