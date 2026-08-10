//! Dispatcher and phase semantics for the interceptor chain.
//!
//! Lived in `src/core/tests/` — beside a *virtual* workspace manifest, which has
//! no package to own it — so cargo never compiled it. See `store.rs`.

use std::cell::RefCell;
use std::rc::Rc;

use jan_klod_core::intercept::{
    BlockReason, Decision, Dispatcher, Driver, HookState, InterceptInput, Interceptor,
    InterceptorError, Outcome, PendingRequest, Phase, ToolCall, UserPrompt, UserTurn,
};

// ---------------------------------------------------------------------------
// Test doubles
// ---------------------------------------------------------------------------

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
    CannedDriver { answer: String::new(), asked: Rc::new(RefCell::new(0)) }
}

fn stub(
    id: &str,
    phases: Vec<Phase>,
    log: &Rc<RefCell<Vec<String>>>,
    behavior: Behavior,
) -> Box<dyn Interceptor> {
    Box::new(Stub { id: id.into(), phases, log: Rc::clone(log), behavior })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

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
    let mut driver = CannedDriver { answer: "yes".into(), asked: Rc::clone(&asked) };
    let mut state = HookState::ToolCall(ToolCall {
        id: "1".into(),
        name: "rm".into(),
        arguments: "{}".into(),
    });
    assert!(matches!(d.dispatch(Phase::ToolCall, &mut state, &mut driver), Outcome::Proceeded));
    assert_eq!(*asked.borrow(), 1, "driver asked exactly once");
    assert_eq!(*log.borrow(), vec!["asker", "asker"]);
}
