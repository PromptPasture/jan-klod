//! A turn writes its own history.
//!
//! Boots the real `Runtime`, runs a turn through `AgentSession`, then reads the
//! `events` table back **through a second `Store` handle on the same file** —
//! not through the session. That is deliberate: the assertion is about what is
//! durable, and reading it through the object that wrote it would prove only
//! that the object remembers.
//!
//! Skips (passes as a no-op) when the guests are not staged in `ext/`.

use jan_klod_core::conductor::Event;
use jan_klod_core::event_log::{self, decode};
use jan_klod_core::store::Store;
use jan_klod_core::Runtime;

use crate::common;

/// A config with a file-backed store, so the log outlives the session and a
/// second handle can read it.
fn config_with_store(dir: &std::path::Path) -> std::path::PathBuf {
    let path = dir.join("config.yaml");
    std::fs::write(
        &path,
        format!(
            "
storage:
  path: {}
extensions:
  provider:
    openai:
      enabled: true
      base-url: http://mock/v1
      model: mock-1
      api-key: test
  interceptor:
    intent-router:
      enabled: true
",
            dir.join("jan-klod.db").display()
        ),
    )
    .unwrap();
    path
}

#[test]
fn a_turn_appends_its_events_in_order() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-eventlog-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = config_with_store(&dir);

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    agent.run("s1", "hello");
    drop(agent);
    drop(runtime);

    let store = Store::open(dir.join("jan-klod.db")).expect("the log is readable afterwards");
    let log = store.session_events("s1").expect("the session has a log");

    assert!(
        !log.is_empty(),
        "a turn that answered wrote nothing — the sink is not wired into the turn path"
    );
    assert_eq!(
        log.iter().map(|row| row.seq).collect::<Vec<_>>(),
        (1..=log.len() as u64).collect::<Vec<_>>(),
        "seq is 1..n with no gaps and no repeats: {:?}",
        log.iter().map(|r| (r.seq, &r.kind)).collect::<Vec<_>>()
    );
    assert!(
        log.iter().all(|row| row.session == "s1"),
        "every row belongs to the session that produced it"
    );

    // The whole shape of a simple turn, pinned rather than described: its own
    // input, the assistant text, the outcome. `text-delta` being here is
    // Acceptance line 2 holding against a real turn and not only against a
    // unit test — a delta reaches the log exactly as it was emitted, uncoalesced.
    assert_eq!(
        log.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>(),
        vec![event_log::KIND_USER_MESSAGE, "text-delta", "done"],
        "the log of one simple turn"
    );

    // The turn's own input opens the log, before anything the model said.
    assert_eq!(log[0].kind, event_log::KIND_USER_MESSAGE);
    assert!(
        log[0].payload.contains("hello"),
        "the message is the one that was sent: {}",
        log[0].payload
    );

    // And the turn's answer closes it. `Done` is the authoritative answer, so a
    // log without it is a log of an unfinished turn.
    let last = log.last().expect("non-empty");
    assert_eq!(last.kind, "done", "the log ends with the turn's outcome");
    let Ok(Event::Done { text, .. }) = decode(&last.kind, &last.payload) else {
        panic!("the final row does not decode as Done: {}", last.payload);
    };
    assert_eq!(text, "pong", "and carries the answer the caller got");
}

/// Two sessions through one agent keep separate logs — the same guarantee the
/// store's unit tests make, asserted here against a real turn because this is
/// where the session id actually comes from.
#[test]
fn each_session_logs_only_its_own_turn() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-eventlog-two-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = config_with_store(&dir);

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    agent.run("first", "hello");
    agent.run("second", "hello again");
    drop(agent);
    drop(runtime);

    let store = Store::open(dir.join("jan-klod.db")).expect("readable");
    for session in ["first", "second"] {
        let log = store.session_events(session).unwrap();
        assert_eq!(log[0].seq, 1, "`{session}` starts its own sequence at 1");
        assert!(
            log.iter().all(|row| row.session == session),
            "`{session}`'s log holds only its own rows"
        );
    }
    let first = store.session_events("first").unwrap();
    assert!(
        first.iter().all(|row| !row.payload.contains("hello again")),
        "the second turn's message did not leak into the first session's log"
    );
}

/// The read surfaces are projections now, so `transcript` and `list_sessions`
/// must answer from the log rather than from the `entries` transcript. Asserted
/// through the public API after a real turn, since that is what `GET
/// /session/:id` and `GET /sessions` serve.
#[test]
fn the_read_surfaces_answer_from_the_log() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-eventlog-read-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = config_with_store(&dir);

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    agent.run("read-me", "what is this?");

    let transcript = agent.transcript("read-me");
    assert_eq!(
        transcript
            .iter()
            .map(|m| m.content.as_str())
            .collect::<Vec<_>>(),
        vec!["what is this?", "pong"],
        "the projected conversation, oldest first"
    );

    assert_eq!(
        agent.list_sessions(),
        vec!["read-me".to_string()],
        "a session with a log is listed — the `entries` namespace is not what makes it visible"
    );
    assert!(
        agent.transcript("never-happened").is_empty(),
        "and a session that never ran reads as empty rather than failing"
    );
}

/// Acceptance line 2: a fork at seq *N* runs its own turn without touching the
/// parent's log.
///
/// Independence is asserted in **both** directions. A fork that shares the
/// parent's history is the easy half; the half that actually breaks is a later
/// turn in one of them leaking into the other, and a fork implemented as a
/// branch pointer rather than a copy would pass the first check and fail this.
#[test]
fn a_fork_runs_its_own_turn_and_leaves_the_parent_alone() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-intent-router.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-eventlog-fork-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = config_with_store(&dir);

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");

    agent.run("parent", "the original question");
    let parent_before = agent.transcript("parent");
    let at_seq = {
        let store = Store::open(dir.join("jan-klod.db")).expect("readable");
        store.session_events("parent").unwrap().len() as u64
    };

    let copied = agent
        .fork_session("parent", at_seq, "child")
        .expect("the fork succeeds");
    assert_eq!(copied, at_seq, "the whole prefix was copied");

    // The fork starts as the parent was.
    assert_eq!(
        agent
            .transcript("child")
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>(),
        parent_before
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>(),
        "the fork opens with the parent's conversation"
    );

    // Then each runs a turn of its own.
    agent.run("child", "only the child asks this");
    agent.run("parent", "only the parent asks this");

    let child = agent.transcript("child");
    let parent = agent.transcript("parent");
    assert!(
        child
            .iter()
            .any(|m| m.content == "only the child asks this"),
        "the fork's own turn is in its log"
    );
    assert!(
        parent
            .iter()
            .all(|m| m.content != "only the child asks this"),
        "and did not reach the parent: {parent:?}"
    );
    assert!(
        child
            .iter()
            .all(|m| m.content != "only the parent asks this"),
        "nor the parent's the fork: {child:?}"
    );
    assert!(
        agent.list_sessions().contains(&"child".to_string()),
        "the fork is a session in its own right"
    );
}

/// Forking a prefix that holds nothing is refused rather than producing a
/// session that silently is not a fork of anything.
#[test]
fn forking_nothing_is_refused() {
    if !common::guests_staged(&["provider-openai.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-eventlog-fork0-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());
    let config = config_with_store(&dir);

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let factory = || common::canned_http("pong");
    let agent = runtime.build_agent(&factory).expect("agent boots");

    assert_eq!(
        agent.fork_session("never-ran", 5, "child").unwrap(),
        0,
        "a session with no log copies nothing, and says so rather than erroring"
    );
    assert!(
        agent.transcript("child").is_empty(),
        "so no fork was created"
    );
}
