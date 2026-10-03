//! A tool asks the user, and the turn carries on (#216).
//!
//! Until now only an interceptor could ask. These drive the other end:
//! `tool-ask-probe` returns a question, the host puts it to the driver, and
//! the tool is invoked again with the answer — the same return-then-reinvoke
//! `interceptor.wit` uses, so no guest is ever on the stack while the loop
//! is suspended.
//!
//! Three cases, and the second and third are the ones that decide whether
//! this is safe to ship: what happens when nobody can answer, and what
//! happens when a tool never stops asking.

use std::cell::RefCell;
use std::rc::Rc;

use jan_klod_core::conductor::ToolInvoker;
use jan_klod_core::host_process::ProcessRunner;
use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::tool_host::{ToolExtension, ToolFleet};
use wasmtime::component::Component;
use wasmtime::Engine;

use crate::common;

const PROBE: &str = "tool-ask-probe.wasm";

/// Answers every question with a fixed reply, and counts them.
struct Answering {
    reply: String,
    asked: Rc<RefCell<Vec<String>>>,
}

impl Driver for Answering {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        self.asked.borrow_mut().push(prompt.question.clone());
        self.reply.clone()
    }
}

/// The driver a headless surface uses: it cannot prompt, so the prompt's
/// own default is the answer. The same type `AgentSession::run` uses.
struct Headless;
impl Driver for Headless {
    fn ask(&mut self, prompt: &UserPrompt) -> String {
        prompt.default_answer.clone()
    }
}

fn fleet() -> Option<ToolFleet> {
    if !common::guests_staged(&[PROBE]) {
        return None;
    }
    let engine = Engine::default();
    let component = Component::from_file(&engine, common::repo_root().join("ext").join(PROBE))
        .expect("the probe compiles");
    let tool = ToolExtension::instantiate_with_http(
        &engine,
        "tool.ask-probe",
        &component,
        None,
        ProcessRunner::disabled(),
        None,
        None,
        false,
    )
    .expect("the probe instantiates");
    Some(ToolFleet::new(vec![tool]))
}

fn call(arguments: &str) -> jan_klod_core::intercept::ToolCall {
    jan_klod_core::intercept::ToolCall {
        id: "c1".to_string(),
        name: "ask-probe".to_string(),
        arguments: arguments.to_string(),
    }
}

/// The ordinary case: the tool asks, the user answers, the tool uses it.
#[test]
fn a_tool_asks_and_is_invoked_again_with_the_answer() {
    let Some(mut fleet) = fleet() else {
        return;
    };
    let asked = Rc::new(RefCell::new(Vec::new()));
    let mut driver = Answering {
        reply: "red".to_string(),
        asked: Rc::clone(&asked),
    };

    let out = fleet
        .invoke_asking(&call("{}"), &mut driver)
        .expect("the probe answers");

    assert_eq!(
        asked.borrow().as_slice(),
        ["which colour?".to_string()],
        "the question did not reach the driver"
    );
    assert!(!out.failed, "{out:?}");
    assert!(
        out.content.contains("\"answer\":\"red\""),
        "the tool was not re-invoked with the answer: {}",
        out.content
    );
}

/// A tool that does not ask is unaffected by any of this.
///
/// The probe exports `tool-askable` and still finishes in one step when
/// told to, so "can ask" and "does ask" stay separable — otherwise the
/// interface would cost a round trip to every call.
#[test]
fn a_tool_that_does_not_ask_answers_in_one_step() {
    let Some(mut fleet) = fleet() else {
        return;
    };
    let asked = Rc::new(RefCell::new(Vec::new()));
    let mut driver = Answering {
        reply: "red".to_string(),
        asked: Rc::clone(&asked),
    };

    let out = fleet
        .invoke_asking(&call(r#"{"skip":true}"#), &mut driver)
        .expect("the probe answers");
    assert!(
        asked.borrow().is_empty(),
        "it asked when it should not have"
    );
    assert!(out.content.contains("\"asked\":false"), "{}", out.content);
}

/// Nobody to ask: the prompt's own default is the answer, and the turn
/// finishes.
///
/// A REST client or an ACP peer has no human behind it. The alternative to
/// a default is a hung turn, which is why `interceptor.wit` put
/// `default-answer` in the prompt and why `tool-askable` reuses that record
/// rather than inventing one without it.
#[test]
fn with_nobody_to_ask_the_default_answers_and_the_turn_finishes() {
    let Some(mut fleet) = fleet() else {
        return;
    };
    let out = fleet
        .invoke_asking(&call("{}"), &mut Headless)
        .expect("the probe answers");
    assert!(!out.failed, "a headless surface failed the call: {out:?}");
    assert!(
        out.content.contains("\"answer\":\"blue\""),
        "the tool did not receive its own default: {}",
        out.content
    );
}

/// A tool that never stops asking is stopped by the host.
///
/// The bound is the host's, not the guest's promise. Without it a component
/// could suspend a turn indefinitely, and the person answering the same
/// question over and over is the one who would find out.
#[test]
fn a_tool_that_asks_forever_is_cut_off_rather_than_hanging_the_turn() {
    let Some(mut fleet) = fleet() else {
        return;
    };
    let asked = Rc::new(RefCell::new(Vec::new()));
    let mut driver = Answering {
        reply: "red".to_string(),
        asked: Rc::clone(&asked),
    };

    let out = fleet
        .invoke_asking(&call(r#"{"forever":true}"#), &mut driver)
        .expect("the call returns rather than hanging");

    assert!(out.failed, "an unbounded asker was reported as success");
    assert!(
        out.content.contains("asked more than"),
        "the failure should say what happened: {}",
        out.content
    );
    let count = asked.borrow().len();
    assert!(
        (1..=8).contains(&count),
        "the user was asked {count} times; the bound is 8"
    );
}
