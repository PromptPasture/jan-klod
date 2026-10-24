//! Two sessions run turns at the same time through the agent registry (#229).
//! A fast turn must finish while a slow turn in another session is still running.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jk_agent::{AgentSession, Runtime};
use jk_host::sessions::{Agents, Factory};

use crate::common;

/// A provider that sleeps when the request asks it to. The delay comes from the
/// request, not the agent, so both sessions get an identical provider.
fn paced_factory(calls: &Arc<AtomicU32>) -> Factory {
    let calls = Arc::clone(calls);
    Arc::new(move || -> jk_wasm::route::HttpFn {
        let calls = Arc::clone(&calls);
        Box::new(move |_m, _u, _h, body, _t| {
            calls.fetch_add(1, Ordering::Relaxed);
            let asked = String::from_utf8_lossy(body.unwrap_or_default()).into_owned();
            if asked.contains("take your time") {
                std::thread::sleep(Duration::from_millis(1500));
            }
            Ok(jk_wasm::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&serde_json::json!({"choices":[{"message":{
                    "role":"assistant","content":"answered"},"finish_reason":"stop"}]}))
                .expect("serialises"),
            })
        })
    })
}

fn booted(tag: &str) -> Option<(common::TempDir, Runtime)> {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-agents-{tag}-{}", std::process::id()));
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
",
            db = dir.join("jan-klod.db").display()
        ),
    )
    .expect("writes the config");
    let runtime =
        Runtime::boot(&config, common::repo_root().join("ext")).expect("the runtime boots");
    Some((guard, runtime))
}

/// A turn in one session does not wait for a turn in another. Asserted on
/// overlap, not order: a single agent could finish the fast turn first by luck.
#[test]
fn two_sessions_run_turns_at_the_same_time() {
    let Some((_guard, runtime)) = booted("concurrent") else {
        return;
    };
    let calls = Arc::new(AtomicU32::new(0));
    let agents = Agents::new(runtime, paced_factory(&calls));

    let slow_done: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let fast_done: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let (started, slow_running) = std::sync::mpsc::channel();

    std::thread::scope(|scope| {
        let slow = agents.of("slow");
        let at = Arc::clone(&slow_done);
        scope.spawn(move || {
            let _ = slow.run(move |agent: &mut AgentSession| {
                let _ = started.send(());
                agent.run("slow", "take your time with this one");
            });
            *at.lock().expect("not poisoned") = Some(Instant::now());
        });

        let fast = agents.of("fast");
        let at = Arc::clone(&fast_done);
        scope.spawn(move || {
            slow_running
                .recv_timeout(Duration::from_secs(60))
                .expect("the slow turn starts");
            let _ = fast.run(|agent: &mut AgentSession| {
                agent.run("fast", "answer now");
            });
            *at.lock().expect("not poisoned") = Some(Instant::now());
        });
    });

    let slow_at = slow_done.lock().expect("not poisoned").expect("slow ran");
    let fast_at = fast_done.lock().expect("not poisoned").expect("fast ran");
    assert!(
        fast_at < slow_at,
        "the fast turn finished after the slow one, so they were serialised"
    );
    assert_eq!(agents.live(), 2, "two sessions, two agents");
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "both turns reached a provider, rather than one answering twice"
    );
}

/// Control for the test above: one agent serialises the same two turns.
#[test]
fn one_agent_makes_the_same_two_turns_wait() {
    let Some((_guard, runtime)) = booted("serialised") else {
        return;
    };
    let calls = Arc::new(AtomicU32::new(0));
    let factory = paced_factory(&calls);
    let (jobs, queued) = jk_host::session_thread::queue::<AgentSession>();

    let slow_ended: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let fast_started: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let (started, slow_running) = std::sync::mpsc::channel();

    std::thread::scope(|scope| {
        let slow = jobs.clone();
        let at = Arc::clone(&slow_ended);
        scope.spawn(move || {
            let _ = slow.run(move |agent: &mut AgentSession| {
                let _ = started.send(());
                agent.run("slow", "take your time with this one");
                *at.lock().expect("not poisoned") = Some(Instant::now());
            });
        });
        let fast = jobs.clone();
        let at = Arc::clone(&fast_started);
        scope.spawn(move || {
            slow_running
                .recv_timeout(Duration::from_secs(60))
                .expect("the slow turn starts");
            let _ = fast.run(move |agent: &mut AgentSession| {
                *at.lock().expect("not poisoned") = Some(Instant::now());
                agent.run("fast", "answer now");
            });
        });
        drop(jobs);

        let mut agent = runtime
            .build_agent(&*factory)
            .expect("the single agent boots");
        jk_host::session_thread::serve(&mut agent, &queued);
    });

    let slow = slow_ended.lock().expect("not poisoned").expect("slow ran");
    let fast = fast_started
        .lock()
        .expect("not poisoned")
        .expect("fast ran");
    assert!(
        fast >= slow,
        "one agent started the fast turn while the slow one was still running"
    );
}

/// Dropping the registry stops every agent loop.
#[test]
fn dropping_the_registry_stops_every_agent() {
    let Some((_guard, runtime)) = booted("shutdown") else {
        return;
    };
    let calls = Arc::new(AtomicU32::new(0));
    let agents = Agents::new(runtime, paced_factory(&calls));

    let kept = agents.of("one");
    agents.of("two");
    agents.housekeeping();
    assert_eq!(agents.live(), 3, "two sessions and the housekeeping agent");

    drop(agents);

    assert!(
        kept.run(|_: &mut AgentSession| ()).is_err(),
        "a queue still answers after its registry was dropped"
    );
}

/// Agents share one store even with no `storage.path` (#233). Before the fix each
/// agent had its own in-memory DB, and REST reads through the housekeeping agent,
/// so sessions went missing silently.
#[test]
fn a_session_written_by_one_agent_is_visible_to_another() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-onestore-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("creates the temp dir");
    let _guard = common::TempDir(dir.clone());
    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        "
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
",
    )
    .expect("writes the config");
    let runtime =
        Runtime::boot(&config, common::repo_root().join("ext")).expect("the runtime boots");
    let agents = Agents::new(runtime, Arc::new(|| common::canned_http("pong")));

    agents
        .of("alpha")
        .run(|agent: &mut AgentSession| {
            agent.run("alpha", "hello");
        })
        .expect("alpha's turn ran");

    let (transcript, listed) = agents
        .housekeeping()
        .run(|agent: &mut AgentSession| {
            (
                agent.transcript("alpha").len(),
                jk_agent::session::sessions_payload(agent).to_string(),
            )
        })
        .expect("the reader ran");

    assert!(
        transcript >= 2,
        "another agent saw {transcript} messages in a session that has a turn"
    );
    assert!(
        listed.contains("alpha"),
        "the session is not in the list every client reads: {listed}"
    );
}
