//! A tool can ask the user mid-call and the turn continues (#216). The probe
//! returns a question and is re-invoked with the answer, so no guest is on the
//! stack while the loop is suspended.

use std::cell::RefCell;
use std::rc::Rc;

use jk_protocol::intercept::ToolInvoker;
use jk_protocol::turn::{Driver, UserPrompt};
use jk_wasm::host_process::ProcessRunner;
use jk_wasm::tool_host::{ToolExtension, ToolFleet};
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

/// Driver for a headless surface, which cannot prompt: the prompt's default is the answer.
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

fn call(arguments: &str) -> jk_protocol::turn::ToolCall {
    jk_protocol::turn::ToolCall {
        id: "c1".to_string(),
        name: "ask-probe".to_string(),
        arguments: arguments.to_string(),
    }
}

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

/// The probe can ask but may finish in one step, so "can ask" and "does ask" stay separate.
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

/// With no one to ask, the prompt's default answers and the turn finishes instead of hanging.
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

/// The host bounds how often a tool may ask; a guest cannot be trusted to stop on its own.
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
