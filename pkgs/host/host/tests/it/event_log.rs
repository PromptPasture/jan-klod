//! A turn writes its own history. Events are read back through a second `Store`

use jk_agent::Runtime;
use jk_protocol::turn::Event;
use jk_session::event_log::{self, decode};
use jk_session::event_log::{verify_chain, ChainStatus};
use jk_session::store::Store;
use rusqlite::params;

use crate::common;

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
        "a turn that answered wrote nothing — the sink is not wired in"
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

    assert_eq!(
        log.iter().map(|row| row.kind.as_str()).collect::<Vec<_>>(),
        vec![event_log::KIND_USER_MESSAGE, "text-delta", "done"],
        "the log of one simple turn"
    );

    assert_eq!(log[0].kind, event_log::KIND_USER_MESSAGE);
    assert!(
        log[0].payload.contains("hello"),
        "the message is the one that was sent: {}",
        log[0].payload
    );

    let last = log.last().expect("non-empty");
    assert_eq!(last.kind, "done", "the log ends with the turn's outcome");
    let Ok(Event::Done { text, .. }) = decode(&last.kind, &last.payload) else {
        panic!("the final row does not decode as Done: {}", last.payload);
    };
    assert_eq!(text, "pong", "and carries the answer the caller got");
}

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

#[test]
fn an_attempted_install_is_recorded_in_the_session_log() {
    if !common::guests_staged(&["provider-openai.wasm", "interceptor-tool-selector.wasm"]) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("jk-install-log-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let _guard = common::TempDir(dir.clone());

    let config = dir.join("config.yaml");
    std::fs::write(
        &config,
        format!(
            "
storage:
  path: {}
registry:
  install-tool: true
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
",
            dir.join("jan-klod.db").display()
        ),
    )
    .unwrap();

    let calls = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
    let factory = move || -> jk_wasm::route::HttpFn {
        let calls = std::sync::Arc::clone(&calls);
        Box::new(move |_m, _u, _h, _b, _t| {
            let n = calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let body = if n == 0 {
                serde_json::json!({"choices":[{"message":{"role":"assistant","tool_calls":[
                    {"id":"c1","function":{"name":"ext-install",
                     "arguments":"{\"path\":\"/nonexistent-probe.wasm\"}"}}]},
                    "finish_reason":"tool_calls"}]})
            } else {
                serde_json::json!({"choices":[{"message":{"role":"assistant",
                    "content":"done"},"finish_reason":"stop"}]})
            };
            Ok(jk_wasm::http::WireResponse {
                status: 200,
                headers: vec![],
                body: serde_json::to_vec(&body).unwrap(),
            })
        })
    };

    let runtime = Runtime::boot(&config, common::repo_root().join("ext")).expect("runtime boots");
    let mut agent = runtime.build_agent(&factory).expect("agent boots");
    agent.run("s1", "install it");
    drop(agent);
    drop(runtime);

    let store = Store::open(dir.join("jan-klod.db")).expect("the log is readable");
    let log = store.session_events("s1").expect("the session has a log");
    let decoded: Vec<Event> = log
        .iter()
        .filter_map(|row| decode(&row.kind, &row.payload).ok())
        .collect();

    let invoked = decoded.iter().any(|event| {
        matches!(event, Event::ToolInvoked(call)
            if call.name == "ext-install" && call.arguments.contains("nonexistent-probe"))
    });
    assert!(
        invoked,
        "the install attempt is not in the log, so nothing records that it \
         happened: {decoded:?}"
    );
    let refusal = decoded.iter().find_map(|event| match event {
        Event::ToolResult(outcome) if outcome.failed => Some(outcome.content.clone()),
        _ => None,
    });
    let refusal = refusal.unwrap_or_else(|| panic!("no failed tool result: {decoded:?}"));
    assert!(
        refusal.contains("nonexistent-probe"),
        "the log records a failure that says nothing about the install — the \
         tool was probably not enabled, and the attempt is unrecorded: \
         {refusal}"
    );
}

#[test]
fn an_edited_event_breaks_the_chain() {
    let dir = std::env::temp_dir().join(format!("jk-chain-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok();

    let store = Store::open(dir.join("test.db")).expect("store opens");
    for i in 1..=5u64 {
        store
            .append_event_at(
                "test-session",
                "test-event",
                &format!("event payload {i}"),
                1000 + i,
            )
            .expect("event appends");
    }

    let conn = rusqlite::Connection::open(dir.join("test.db")).expect("connection opens");
    conn.execute(
        "UPDATE events SET payload = ? WHERE session = ? AND seq = ?",
        params!["TAMPERED payload 3", "test-session", 3u64],
    )
    .expect("payload is updated");
    drop(conn);
    drop(store);

    let store = Store::open(dir.join("test.db")).expect("store re-opens");
    let result = verify_chain(&store, "test-session").expect("verify_chain succeeds");

    match result.status {
        ChainStatus::Intact => panic!("chain should be broken after payload edit"),
        ChainStatus::Broken { seq, .. } => {
            assert_eq!(seq, 4, "chain breaks at the event after the tampered one");
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_reordered_pair_breaks_the_chain() {
    let dir = std::env::temp_dir().join(format!("jk-chain-reorder-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok();

    let store = Store::open(dir.join("test.db")).expect("store opens");
    for i in 1..=5u64 {
        store
            .append_event_at(
                "test-session",
                "test-event",
                &format!("event payload {i}"),
                2000 + i,
            )
            .expect("event appends");
    }

    let conn = rusqlite::Connection::open(dir.join("test.db")).expect("connection opens");
    conn.execute("BEGIN TRANSACTION", []).ok();
    let (_seq3, _ts3, _kind3, _payload3, _pd3) = conn
        .query_row(
            "SELECT seq, ts, kind, payload, prev_digest FROM events WHERE session = ? AND seq = ?",
            params!["test-session", 3u64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .expect("row 3 retrieved");
    let (_seq4, _ts4, _kind4, _payload4, _pd4) = conn
        .query_row(
            "SELECT seq, ts, kind, payload, prev_digest FROM events WHERE session = ? AND seq = ?",
            params!["test-session", 4u64],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .expect("row 4 retrieved");

    conn.execute(
        "UPDATE events SET seq = 999999 WHERE session = ? AND seq = ?",
        params!["test-session", 3i64],
    )
    .expect("row 3 temp update");
    conn.execute(
        "UPDATE events SET seq = 3 WHERE session = ? AND seq = ?",
        params!["test-session", 4i64],
    )
    .expect("row 4 moved to 3");
    conn.execute(
        "UPDATE events SET seq = 4 WHERE session = ? AND seq = ?",
        params!["test-session", 999_999_i64],
    )
    .expect("row 3 moved to 4");
    conn.execute("COMMIT", []).ok();
    drop(conn);
    drop(store);

    let store = Store::open(dir.join("test.db")).expect("store re-opens");
    let result = verify_chain(&store, "test-session").expect("verify_chain succeeds");

    match result.status {
        ChainStatus::Intact => panic!("chain should be broken after reordering"),
        ChainStatus::Broken { seq, .. } => {
            assert!(
                seq == 3 || seq == 4,
                "chain breaks at one of the swapped events"
            );
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn a_deleted_event_breaks_the_chain() {
    let dir = std::env::temp_dir().join(format!("jk-chain-delete-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok();

    let store = Store::open(dir.join("test.db")).expect("store opens");
    for i in 1..=5u64 {
        store
            .append_event_at(
                "test-session",
                "test-event",
                &format!("event payload {i}"),
                3000 + i,
            )
            .expect("event appends");
    }

    let conn = rusqlite::Connection::open(dir.join("test.db")).expect("connection opens");
    conn.execute(
        "DELETE FROM events WHERE session = ? AND seq = ?",
        params!["test-session", 3u64],
    )
    .expect("event deleted");
    drop(conn);
    drop(store);

    let store = Store::open(dir.join("test.db")).expect("store re-opens");
    let result = verify_chain(&store, "test-session").expect("verify_chain succeeds");

    match result.status {
        ChainStatus::Intact => panic!("chain should be broken after deletion"),
        ChainStatus::Broken { seq, .. } => {
            assert_eq!(seq, 4, "chain breaks at the event after the deleted one");
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn an_intact_chain_verifies_cleanly() {
    let dir = std::env::temp_dir().join(format!("jk-chain-intact-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok();

    let store = Store::open(dir.join("test.db")).expect("store opens");
    for i in 1..=5u64 {
        store
            .append_event_at(
                "test-session",
                "test-event",
                &format!("event payload {i}"),
                4000 + i,
            )
            .expect("event appends");
    }
    drop(store);

    let store = Store::open(dir.join("test.db")).expect("store re-opens");
    let result = verify_chain(&store, "test-session").expect("verify_chain succeeds");

    match result.status {
        ChainStatus::Intact => {}
        ChainStatus::Broken { seq, kind, reason } => {
            panic!("chain should be intact but is broken at seq={seq}, kind={kind}: {reason}");
        }
    }

    std::fs::remove_dir_all(&dir).ok();
}
