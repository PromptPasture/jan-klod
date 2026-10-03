//! How far a standing `always` reaches (#232).
//!
//! The docs now say: by default a grant lasts for the session that gave
//! it, and `persist: true` shares it across every session as well as
//! across restarts. Those are two different claims about scope, and a
//! sentence about scope is worth exactly as much as the test under it —
//! so both are asserted here, and the `persist` half is asserted
//! *because* it is the one an operator deliberately turned on.
//!
//! The observable is **whether the gate asked**, counted by the driver.
//! A grant that is remembered means no question; one that is not means
//! the same question again.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jan_klod_core::conductor::{Event, EventSink, Flow};
use jan_klod_core::intercept::{Driver, UserPrompt};
use jan_klod_core::{AgentSession, Runtime};
use jan_klod_host::sessions::Agents;

use crate::common;

const GUESTS: [&str; 3] = [
    "provider-openai.wasm",
    "interceptor-tool-selector.wasm",
    "interceptor-permission.wasm",
];

/// Answers every confirmation with `always`, and counts them.
struct AlwaysAllow {
    asked: Arc<AtomicU32>,
}

impl Driver for AlwaysAllow {
    fn ask(&mut self, _prompt: &UserPrompt) -> String {
        self.asked.fetch_add(1, Ordering::Relaxed);
        "always".to_string()
    }
}

/// A sink that keeps nothing: this test is about the questions, not the
/// answers.
struct Quiet;
impl EventSink for Quiet {
    fn emit(&mut self, _event: &Event) -> Flow {
        Flow::Continue
    }
}

/// A fleet whose model reaches for a gated tool on the first completion
/// of every turn, so each turn is a chance to ask.
fn agents_with_permission(tag: &str, persist: bool) -> Option<(common::TempDir, Arc<Agents>)> {
    if !common::guests_staged(&GUESTS) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-permscope-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the temp dir");
    let guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
storage:
  path: {db}
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    tool-selector:
      enabled: true
    permission:
      enabled: true
      persist: {persist}
",
            db = dir.join("jan-klod.db").display()
        ),
    )
    .expect("writes the config");

    let factory = Arc::new(|| -> jan_klod_core::route::HttpFn {
        Box::new(move |_m, _u, _h, body, _t| {
            let asked = String::from_utf8_lossy(body.unwrap_or_default()).into_owned();
            // A tool call on the first completion of a turn; an answer
            // once its result is in the history.
            let reply = if asked.contains("tool_call_id") {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"all done"},"finish_reason":"stop"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"bash","arguments":"{}"}}]},
                    "finish_reason":"tool_calls"}]})
            };
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&reply).expect("serialises"),
            })
        })
    });
    let runtime =
        Runtime::boot(&config, common::repo_root().join("ext")).expect("the runtime boots");
    Some((guard, Agents::new(runtime, factory)))
}

/// Run one turn in `session`, answering any confirmation with `always`.
fn turn(agents: &Agents, session: &str, asked: &Arc<AtomicU32>) {
    let asked = Arc::clone(asked);
    let session = session.to_owned();
    agents
        .of(&session.clone())
        .run(move |agent: &mut AgentSession| {
            let mut driver = AlwaysAllow { asked };
            agent.run_streaming_with_driver(&mut driver, &mut Quiet, &session, "use bash to tidy");
        })
        .expect("the turn ran");
}

/// The default: a grant lasts for the session that gave it.
///
/// Three turns and two sessions. The second turn in the *same* session
/// must not ask — otherwise "always" means nothing and this test would
/// pass for the wrong reason, since a gate that always asks would also
/// ask the second session.
#[test]
fn a_standing_grant_lasts_for_the_session_that_gave_it() {
    let Some((_guard, agents)) = agents_with_permission("default", false) else {
        return;
    };
    let asked = Arc::new(AtomicU32::new(0));

    turn(&agents, "first", &asked);
    let after_one = asked.load(Ordering::Relaxed);
    assert_eq!(after_one, 1, "the gate did not ask at all");

    turn(&agents, "first", &asked);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "`always` was not remembered inside the session that said it"
    );

    turn(&agents, "second", &asked);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        2,
        "a second session inherited a grant it never gave"
    );
}

/// And `persist: true` shares it, which is the half the documentation
/// would otherwise get wrong for the operator who turned it on.
///
/// Same script, same three turns, one config key different.
#[test]
fn persist_true_shares_the_grant_with_every_session() {
    let Some((_guard, agents)) = agents_with_permission("persist", true) else {
        return;
    };
    let asked = Arc::new(AtomicU32::new(0));

    turn(&agents, "first", &asked);
    assert_eq!(asked.load(Ordering::Relaxed), 1, "the gate did not ask");

    turn(&agents, "first", &asked);
    turn(&agents, "second", &asked);
    assert_eq!(
        asked.load(Ordering::Relaxed),
        1,
        "with `persist: true` a second session was asked again, so the grant is \
         not shared and the documentation is wrong rather than this test"
    );
}
