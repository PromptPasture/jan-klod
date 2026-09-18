//! Two sessions, two agents, two turns at the same time (#229).
//!
//! The property the registry exists for, and the one that cannot be
//! faked: a slow turn in one session and a fast turn in another, started
//! together, with the fast one finishing while the slow one is still
//! going. With a single agent the fast turn waits — which is the
//! behaviour Phase 20's gate was filed against.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jan_klod_core::{AgentSession, Runtime};
use jan_klod_host::sessions::{Agents, Factory};

use crate::common;

/// A provider that takes its time when the message says to.
///
/// The delay is chosen by the *request*, not by the agent, so both
/// sessions get an identical provider and the difference between them is
/// only what they were asked to do. An agent-specific slow provider would
/// prove nothing about concurrency.
fn paced_factory(calls: &Arc<AtomicU32>) -> Factory {
    let calls = Arc::clone(calls);
    Arc::new(move || -> jan_klod_core::route::HttpFn {
        let calls = Arc::clone(&calls);
        Box::new(move |_m, _u, _h, body, _t| {
            calls.fetch_add(1, Ordering::Relaxed);
            let asked = String::from_utf8_lossy(body.unwrap_or_default()).into_owned();
            if asked.contains("take your time") {
                std::thread::sleep(Duration::from_millis(1500));
            }
            Ok(jan_klod_core::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&serde_json::json!({"choices":[{"message":{
                    "role":"assistant","content":"answered"},"finish_reason":"stop"}]}))
                .expect("serialises"),
            })
        })
    })
}

fn booted(tag: &str) -> Option<(common::TempDir, Arc<Runtime>)> {
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
    Some((guard, Arc::new(runtime)))
}

/// The point of the slice: a turn in one session does not wait for a turn
/// in another.
///
/// Asserted on **overlap**, not on ordering. "The fast one finished
/// first" is satisfied by luck on a single agent if the slow turn happens
/// to be queued second; "the fast one finished while the slow one was
/// still running" is not.
#[test]
fn two_sessions_run_turns_at_the_same_time() {
    let Some((_guard, runtime)) = booted("concurrent") else {
        return;
    };
    let calls = Arc::new(AtomicU32::new(0));
    let agents = Agents::new(Arc::clone(&runtime), paced_factory(&calls));

    let slow_done: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let fast_done: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    std::thread::scope(|scope| {
        let slow = agents.of("slow");
        let at = Arc::clone(&slow_done);
        scope.spawn(move || {
            let _ = slow.run(|agent: &mut AgentSession| {
                agent.run("slow", "take your time with this one");
            });
            *at.lock().expect("not poisoned") = Some(Instant::now());
        });

        // Far enough behind that the slow turn is certainly in flight,
        // and far short of the 1.5 s it takes.
        std::thread::sleep(Duration::from_millis(300));

        let fast = agents.of("fast");
        let at = Arc::clone(&fast_done);
        scope.spawn(move || {
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

/// Its control: the same two turns against **one** agent serialise.
///
/// Without this, the test above passes on a machine fast enough to make
/// any ordering look like overlap. This is the behaviour being replaced,
/// asserted so that the replacement means something.
#[test]
fn one_agent_makes_the_same_two_turns_wait() {
    let Some((_guard, runtime)) = booted("serialised") else {
        return;
    };
    let calls = Arc::new(AtomicU32::new(0));
    let factory = paced_factory(&calls);
    let (jobs, queued) = jan_klod_host::session_thread::queue::<AgentSession>();

    let fast_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));
    let slow_at: Arc<Mutex<Option<Instant>>> = Arc::new(Mutex::new(None));

    std::thread::scope(|scope| {
        let slow = jobs.clone();
        let at = Arc::clone(&slow_at);
        scope.spawn(move || {
            let _ = slow.run(|agent: &mut AgentSession| {
                agent.run("slow", "take your time with this one");
            });
            *at.lock().expect("not poisoned") = Some(Instant::now());
        });
        std::thread::sleep(Duration::from_millis(300));
        let fast = jobs.clone();
        let at = Arc::clone(&fast_at);
        scope.spawn(move || {
            let _ = fast.run(|agent: &mut AgentSession| {
                agent.run("fast", "answer now");
            });
            *at.lock().expect("not poisoned") = Some(Instant::now());
        });
        drop(jobs);

        let mut agent = runtime
            .build_agent(&*factory)
            .expect("the single agent boots");
        jan_klod_host::session_thread::serve(&mut agent, &queued);
    });

    let slow = slow_at.lock().expect("not poisoned").expect("slow ran");
    let fast = fast_at.lock().expect("not poisoned").expect("fast ran");
    assert!(
        fast > slow,
        "one agent served both turns at once, which it cannot do"
    );
}

/// Dropping the registry ends every thread, and waiting for them is what
/// makes that observable.
#[test]
fn dropping_the_registry_stops_every_agent() {
    let Some((_guard, runtime)) = booted("shutdown") else {
        return;
    };
    let calls = Arc::new(AtomicU32::new(0));
    let agents = Agents::new(Arc::clone(&runtime), paced_factory(&calls));

    let kept = agents.of("one");
    agents.of("two");
    agents.housekeeping();
    assert_eq!(agents.live(), 3, "two sessions and the housekeeping agent");

    drop(agents);

    // The handle outlived the registry; its loop did not.
    assert!(
        kept.run(|_: &mut AgentSession| ()).is_err(),
        "a queue still answers after its registry was dropped"
    );
}
