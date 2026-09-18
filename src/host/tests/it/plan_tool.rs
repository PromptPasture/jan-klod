//! `tool-plan` across turns and across sessions.
//!
//! The guest's own tests (`src/extensions/tool-plan/src/plan.rs`) cover every
//! rule about what a plan *is*. These cover the two things that can only be
//! true of the whole stack: a plan written in one turn is there in the next,
//! and two sessions do not see each other's.
//!
//! The second is the one worth the machinery. `scope: session` is host-side
//! (#215), the guest holds no session id, and nothing in the guest's unit
//! tests can tell whether the host actually keyed by session — only a second
//! session can.

use std::sync::{Arc, Mutex};

use jan_klod_core::Runtime;

use crate::common;

const GUESTS: [&str; 3] = [
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "tool-plan.wasm",
];

/// What the model "decides" to send to `plan`, one entry per turn, and what
/// came back.
struct Script {
    /// Tool-call arguments, popped in order as turns run.
    calls: Mutex<Vec<String>>,
    /// One entry per turn that ran a tool: the request carrying its result.
    seen: Mutex<Vec<String>>,
}

impl Script {
    fn new(calls: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            // Reversed so a turn can `pop` the next one cheaply.
            calls: Mutex::new(calls.iter().rev().map(|s| (*s).to_string()).collect()),
            seen: Mutex::new(Vec::new()),
        })
    }

    /// One entry per turn that ran the tool, in order.
    fn turns(&self) -> Vec<String> {
        self.seen.lock().expect("not poisoned").clone()
    }
}

/// A provider that calls `plan` once per turn, then ends the turn.
///
/// **The turn boundary is read from the transcript, not counted.** Counting
/// completions was the first attempt and it was wrong: a turn is two
/// completions, so a counter made one turn consume two scripted calls and
/// the next turn none. The request body carries the whole history, so the
/// role of its last message says which half of a turn this is — a fresh user
/// message means "call the tool", a tool result means "answer and stop".
fn provider(script: &Arc<Script>) -> impl Fn() -> jan_klod_core::route::HttpFn {
    let script = Arc::clone(script);
    move || -> jan_klod_core::route::HttpFn {
        let script = Arc::clone(&script);
        Box::new(move |_m, _u, _h, body, _t| {
            let text = body.map(|b| String::from_utf8_lossy(b).into_owned());
            let after_tool = text.as_deref().is_some_and(|body| {
                serde_json::from_str::<serde_json::Value>(body)
                    .ok()
                    .and_then(|v| {
                        v.get("messages")?
                            .as_array()?
                            .last()?
                            .get("role")?
                            .as_str()
                            .map(str::to_owned)
                    })
                    .as_deref()
                    == Some("tool")
            });

            let response = if after_tool {
                // Record what the tool actually returned, which is the last
                // message in this request.
                if let Some(body) = text {
                    script.seen.lock().expect("not poisoned").push(body);
                }
                serde_json::json!({"choices":[{"message":{
                    "role":"assistant","content":"done"},"finish_reason":"stop"}]})
            } else {
                let arguments = script
                    .calls
                    .lock()
                    .expect("not poisoned")
                    .pop()
                    .unwrap_or_else(|| r#"{"op":"list"}"#.to_string());
                serde_json::json!({"choices":[{"message":{
                    "role":"assistant","tool_calls":[{"id":"c1","function":{
                        "name":"plan","arguments":arguments}}]},
                    "finish_reason":"tool_calls"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&response).unwrap(),
            })
        })
    }
}

/// A config enabling `tool.plan`, with session scoping as the issue requires.
fn config_in(dir: &std::path::Path, scope_session: bool) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    let scope = if scope_session {
        "      scope: session\n"
    } else {
        ""
    };
    std::fs::write(
        &path,
        format!(
            "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  tool:
    plan:
      enabled: true
{scope}  interceptor:
    tool-selector:
      enabled: true
"
        ),
    )
    .expect("writes the config");
    path
}

/// A plan written in one turn is still there in the next.
#[test]
fn a_plan_written_in_one_turn_is_read_back_in_the_next() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let dir = common::TempDir(
        std::env::temp_dir().join(format!("jk-plan-across-turns-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let config = config_in(&dir.0, true);

    let script = Script::new(&[
        r#"{"op":"add","text":"read the issue"}"#,
        r#"{"op":"list"}"#,
    ]);
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let mut agent = runtime
        .build_agent(&provider(&script))
        .expect("agent boots");

    agent.run("s1", "plan the work");
    agent.run("s1", "what was the plan");

    let turns = script.turns();
    assert_eq!(turns.len(), 2, "both turns ran the tool: {turns:?}");
    assert!(
        turns[1].contains("read the issue"),
        "the second turn listed the plan and did not see the first turn's \
         step: {}",
        turns[1]
    );
}

/// Two sessions do not share a plan.
///
/// This is the whole reason `scope: session` exists, and it cannot be tested
/// anywhere below here: the guest has no session id, so from inside it the
/// scoped and unscoped cases are identical.
#[test]
fn two_sessions_do_not_share_a_plan() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let dir = common::TempDir(
        std::env::temp_dir().join(format!("jk-plan-sessions-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let config = config_in(&dir.0, true);

    let script = Script::new(&[r#"{"op":"add","text":"belongs to s1"}"#, r#"{"op":"list"}"#]);
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let mut agent = runtime
        .build_agent(&provider(&script))
        .expect("agent boots");

    agent.run("s1", "plan the work");
    agent.run("s2", "what is my plan");

    let turns = script.turns();
    assert_eq!(turns.len(), 2, "both turns ran the tool: {turns:?}");
    // Each request carries its own session's history only, so s2's body is
    // the whole evidence — if the host did not key by session, s1's step
    // would be in it.
    assert!(
        !turns[1].contains("belongs to s1"),
        "s2 was shown s1's plan: {}",
        turns[1]
    );
    assert!(
        turns[1].contains(r#"steps\":[]"#),
        "s2 should have seen an empty plan of its own: {}",
        turns[1]
    );
}

/// Without `scope: session`, the two sessions *do* share — which is what
/// makes the test above mean something.
///
/// A passing isolation test proves nothing on its own: it passes just as
/// well against a runtime where the second session saw an empty plan because
/// the tool was broken, or because nothing was ever written. This is the
/// control. It also pins the default, which several other guests depend on:
/// storage is run-scoped unless an instance asks otherwise.
#[test]
fn without_session_scope_the_same_two_sessions_share_a_plan() {
    if !common::guests_staged(&GUESTS) {
        return;
    }
    let dir = common::TempDir(
        std::env::temp_dir().join(format!("jk-plan-unscoped-{}", std::process::id())),
    );
    std::fs::create_dir_all(&dir.0).expect("creates the temp dir");
    let config = config_in(&dir.0, false);

    let script = Script::new(&[r#"{"op":"add","text":"belongs to s1"}"#, r#"{"op":"list"}"#]);
    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let mut agent = runtime
        .build_agent(&provider(&script))
        .expect("agent boots");

    agent.run("s1", "plan the work");
    agent.run("s2", "what is my plan");

    let turns = script.turns();
    assert_eq!(turns.len(), 2, "both turns ran the tool: {turns:?}");
    assert!(
        turns[1].contains("belongs to s1"),
        "unscoped storage stopped being shared across sessions, so the \
         isolation test above no longer proves anything: {}",
        turns[1]
    );
}
