//! A real `AgentSession` served through the seam (#223), from threads that never
//! hold it. `AgentSession` is `!Send`, so a thread that tried to hold one would
//! not compile.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use jk_agent::{AgentSession, Runtime};
use jk_host::session_thread::{queue, serve};

use crate::common;

/// A provider and a store only: this tests threading, not turns.
fn booted(tag: &str) -> Option<(common::TempDir, AgentSession, Arc<AtomicU32>)> {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("jk-seam-{tag}-{}", std::process::id()));
    let ext = dir.join("ext");
    std::fs::create_dir_all(&ext).expect("creates the ext dir");
    let guard = common::TempDir(dir.clone());
    let staged = common::repo_root().join("ext");
    for suffix in [".wasm", ".manifest.toml"] {
        let name = format!("provider-openai{suffix}");
        std::fs::copy(staged.join(&name), ext.join(&name)).expect("stages the provider");
    }

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

    // Counts calls, so "both ran" means the provider was hit twice.
    let completions = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&completions);
    let factory = move || -> jk_wasm::route::HttpFn {
        let counter = Arc::clone(&counter);
        Box::new(move |_m, _u, _h, _b, _t| {
            counter.fetch_add(1, Ordering::Relaxed);
            Ok(jk_wasm::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&serde_json::json!({"choices":[{"message":{
                    "role":"assistant","content":"answered"},"finish_reason":"stop"}]}))
                .expect("serialises"),
            })
        })
    };

    let runtime = Runtime::boot(&config, &ext).expect("the runtime boots");
    let agent = runtime.build_agent(&factory).expect("the agent boots");
    Some((guard, agent, completions))
}

/// Two turns in two sessions, queued from two threads that each hold only a `Jobs`
/// handle. `serve_authed` cannot do this: its accepting thread would own the session.
#[test]
fn two_threads_queue_turns_in_two_sessions_without_holding_the_session() {
    let Some((_guard, mut agent, completions)) = booted("two-turns") else {
        return;
    };
    let (jobs, queued) = queue::<AgentSession>();

    // Spawn eagerly: a lazy iterator would run after the loop had already ended.
    let mut callers = Vec::new();
    for session in ["s1", "s2"] {
        let jobs = jobs.clone();
        callers.push(std::thread::spawn(move || {
            jobs.run(move |agent: &mut AgentSession| {
                format!("{:?}", agent.run(session, "say something"))
            })
        }));
    }
    // The loop ends when the last handle goes; the callers hold clones.
    drop(jobs);

    serve(&mut agent, &queued);

    let mut answers: Vec<String> = Vec::new();
    for caller in callers {
        answers.push(caller.join().expect("the caller finished").expect("served"));
    }
    assert_eq!(answers.len(), 2);
    for answer in &answers {
        assert!(
            answer.contains("answered"),
            "a queued turn did not reach the provider: {answer}"
        );
    }
    assert_eq!(
        completions.load(Ordering::Relaxed),
        2,
        "both turns must have called the provider, not one turn answered twice"
    );
}

/// Each turn lands in its own session. Answers are identical by construction, so
/// this reads the transcripts back by id.
#[test]
fn each_queued_turn_landed_in_its_own_session() {
    let Some((_guard, mut agent, _)) = booted("two-sessions") else {
        return;
    };
    let (jobs, queued) = queue::<AgentSession>();

    let mut callers = Vec::new();
    for session in ["alpha", "beta"] {
        let jobs = jobs.clone();
        callers.push(std::thread::spawn(move || {
            jobs.run(move |agent: &mut AgentSession| {
                agent.run(session, "say something");
            })
        }));
    }
    drop(jobs);
    serve(&mut agent, &queued);
    for caller in callers {
        caller.join().expect("finished").expect("served");
    }

    for session in ["alpha", "beta"] {
        let transcript = agent.transcript(session);
        assert!(
            !transcript.is_empty(),
            "`{session}` has no transcript, so its turn went somewhere else"
        );
    }
}
