//! `tool-plan` across turns and sessions. Plan rules are covered by the guest's
//! own tests; these cover what only the whole stack can show.

use std::sync::{Arc, Mutex};

use jk_agent::Runtime;

use crate::common;

const GUESTS: [&str; 3] = [
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "tool-plan.wasm",
];

/// Scripted `plan` arguments and the requests that carried their results.
struct Script {
    calls: Mutex<Vec<String>>,
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

    fn turns(&self) -> Vec<String> {
        self.seen.lock().expect("not poisoned").clone()
    }
}

/// Calls `plan` once per turn, then ends it. The turn half is read from the
/// last message's role: a turn is two completions, so counting them misaligns.
fn provider(script: &Arc<Script>) -> impl Fn() -> jk_wasm::route::HttpFn {
    let script = Arc::clone(script);
    move || -> jk_wasm::route::HttpFn {
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
            Ok(jk_wasm::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&response).unwrap(),
            })
        })
    }
}

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

/// Only a second session can show that the host keys storage by session: the
/// guest has no session id.
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
    // If the host ignored the session key, s1's step would appear here.
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

/// Control for the isolation test: without `scope: session` the sessions share
/// a plan. Also pins the run-scoped default that other guests rely on.
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
