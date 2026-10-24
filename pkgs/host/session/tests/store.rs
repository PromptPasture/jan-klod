//! The host-side persistent store.

use jk_protocol::turn::{Event, ToolCall, ToolOutcome};
use jk_session::event_log::{encode, envelope, KIND_USER_MESSAGE};
use jk_session::projection;
use jk_session::store::{Store, StoreError};

#[test]
fn set_get_roundtrip_and_missing_is_not_found() {
    let store = Store::open_in_memory().unwrap();
    let entry = store.set("hist", "turn-1", "{\"x\":1}").unwrap();
    assert_eq!(entry.namespace, "hist");
    assert_eq!(entry.key, "turn-1");
    assert_eq!(entry.value, "{\"x\":1}");
    assert_eq!(entry.id, "hist/turn-1");

    assert_eq!(store.get("hist", "turn-1").unwrap().value, "{\"x\":1}");
    assert_eq!(store.get("hist", "absent"), Err(StoreError::NotFound));
}

#[test]
fn upsert_preserves_created_at() {
    let store = Store::open_in_memory().unwrap();
    let first = store.set("ns", "k", "a").unwrap();
    let second = store.set("ns", "k", "b").unwrap();
    assert_eq!(second.value, "b");
    assert_eq!(
        second.created_at, first.created_at,
        "created_at is stable across updates"
    );
    assert!(second.updated_at >= first.updated_at);
}

#[test]
fn delete_is_a_noop_when_absent() {
    let store = Store::open_in_memory().unwrap();
    assert!(store.delete("ns", "absent").is_ok());
    store.set("ns", "k", "v").unwrap();
    store.delete("ns", "k").unwrap();
    assert_eq!(store.get("ns", "k"), Err(StoreError::NotFound));
}

#[test]
fn list_keys_omits_value_and_recent_orders_newest_first() {
    let store = Store::open_in_memory().unwrap();
    store.set("ns", "a", "va").unwrap();
    store.set("ns", "b", "vb").unwrap();
    let keys = store.list_keys("ns").unwrap();
    assert_eq!(keys.len(), 2);
    assert!(
        keys.iter().all(|e| e.value.is_empty()),
        "list-keys omits the payload"
    );

    let recent = store.recent("ns", 1).unwrap();
    assert_eq!(recent.len(), 1);
    assert_eq!(recent[0].key, "b", "most recently written comes first");
}

#[test]
fn search_matches_value_substring() {
    let store = Store::open_in_memory().unwrap();
    store.set("ns", "a", "hello world").unwrap();
    store.set("ns", "b", "goodbye").unwrap();
    let hits = store.search("ns", "world", 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].key, "a");
}

#[test]
fn purge_namespace_clears_only_that_namespace() {
    let store = Store::open_in_memory().unwrap();
    store.set("a", "k", "1").unwrap();
    store.set("b", "k", "2").unwrap();
    store.purge_namespace("a").unwrap();
    assert_eq!(store.get("a", "k"), Err(StoreError::NotFound));
    assert_eq!(store.get("b", "k").unwrap().value, "2");
}

#[test]
fn state_survives_a_reopen() {
    let path = std::env::temp_dir().join(format!("jk-store-integ-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&path);
    {
        let store = Store::open(&path).unwrap();
        store.set("session", "history", "durable").unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(store.get("session", "history").unwrap().value, "durable");
    }
    std::fs::remove_file(&path).ok();
}

#[test]
fn backend_error_carries_detail() {
    let result = Store::open("/nonexistent/deep/path/cannot/exist/db.sqlite");
    let Err(StoreError::Backend { detail }) = result else {
        panic!("expected Err(Backend), got Ok or a different error variant");
    };
    assert!(
        !detail.is_empty(),
        "detail must describe the failure, got empty string"
    );
}

#[test]
fn namespaces_list_most_recently_written_first() {
    let store = Store::open_in_memory().unwrap();
    store.set("alpha", "k", "1").unwrap();
    store.set("beta", "k", "1").unwrap();
    store.set("alpha", "k2", "2").unwrap();

    let namespaces = store.list_namespaces().expect("the query is valid SQL");
    assert_eq!(
        namespaces.len(),
        2,
        "one row per namespace, not per entry: {namespaces:?}"
    );
    assert!(namespaces.contains(&"alpha".to_string()));
    assert!(namespaces.contains(&"beta".to_string()));
}

#[test]
fn appends_are_numbered_from_one_and_in_order() {
    let store = Store::open_in_memory().unwrap();
    let first = store
        .append_event("s1", "text-delta", r#"{"text":"a"}"#)
        .unwrap();
    let second = store
        .append_event("s1", "done", r#"{"answer":"b"}"#)
        .unwrap();
    assert_eq!((first.seq, second.seq), (1, 2));
    assert_eq!(first.session, "s1");
    assert_eq!(first.kind, "text-delta");
    assert_eq!(second.payload, r#"{"answer":"b"}"#);

    let log = store.session_events("s1").unwrap();
    assert_eq!(log, vec![first, second], "read back in seq order");
}

/// Each session numbers its own log, so a log reads as a gap-free sequence for replay.
#[test]
fn each_session_has_its_own_sequence() {
    let store = Store::open_in_memory().unwrap();
    store.append_event("a", "k", "{}").unwrap();
    store.append_event("b", "k", "{}").unwrap();
    let second_of_a = store.append_event("a", "k", "{}").unwrap();
    let first_of_b = store.session_events("b").unwrap();

    assert_eq!(second_of_a.seq, 2);
    assert_eq!(first_of_b.len(), 1);
    assert_eq!(first_of_b[0].seq, 1, "session b starts at 1, not 3");
}

#[test]
fn a_session_with_no_events_reads_as_empty_rather_than_missing() {
    let store = Store::open_in_memory().unwrap();
    assert_eq!(store.session_events("never-used").unwrap(), Vec::new());
}

/// Sorts past the point where string order diverges from numeric order (`"10" < "9"`).
#[test]
fn ordering_holds_past_single_digits() {
    let store = Store::open_in_memory().unwrap();
    for i in 0..12 {
        store
            .append_event("s", "k", &format!("{{\"i\":{i}}}"))
            .unwrap();
    }
    let seqs: Vec<u64> = store
        .session_events("s")
        .unwrap()
        .iter()
        .map(|e| e.seq)
        .collect();
    assert_eq!(seqs, (1..=12).collect::<Vec<u64>>());
}

#[test]
fn purging_a_session_leaves_other_sessions_and_the_entries_table_alone() {
    let store = Store::open_in_memory().unwrap();
    store.set("ns", "k", "v").unwrap();
    store.append_event("doomed", "k", "{}").unwrap();
    store.append_event("kept", "k", "{}").unwrap();

    store.purge_session_events("doomed").unwrap();

    assert_eq!(store.session_events("doomed").unwrap(), Vec::new());
    assert_eq!(store.session_events("kept").unwrap().len(), 1);
    assert_eq!(store.get("ns", "k").unwrap().value, "v");
}

/// The log survives a reopen, and a database created before the `events` table gains it on open
/// (`CREATE TABLE IF NOT EXISTS`); without that, every deployed `jan-klod.db` would fail on append.
#[test]
fn an_existing_database_gains_the_table_and_keeps_the_log() {
    let dir = std::env::temp_dir().join(format!(
        "jk-store-events-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("jan-klod.db");

    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE entries (
                namespace  TEXT    NOT NULL,
                key        TEXT    NOT NULL,
                value      TEXT    NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (namespace, key)
            );
            INSERT INTO entries VALUES ('ns', 'k', 'v', 0, 0);",
        )
        .unwrap();
    }
    {
        let store = Store::open(&path).unwrap();
        let appended = store.append_event("s", "k", "{}").unwrap();
        assert_eq!(appended.seq, 1, "the table was recreated on open");
        assert_eq!(store.get("ns", "k").unwrap().value, "v", "entries survived");
    }
    {
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.session_events("s").unwrap().len(),
            1,
            "the log survives a reopen"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

/// Sessions with a log even when they have no entries, which `list_namespaces` would miss.
#[test]
fn sessions_with_events_are_listed_even_with_no_entries() {
    let store = Store::open_in_memory().unwrap();
    store.append_event("logged", "k", "{}").unwrap();

    assert_eq!(store.event_sessions().unwrap(), vec!["logged".to_string()]);
    assert!(
        store.list_namespaces().unwrap().is_empty(),
        "and `list_namespaces` sees nothing, which is the trap this avoids"
    );
}

#[test]
fn event_sessions_lists_each_session_once() {
    let store = Store::open_in_memory().unwrap();
    store.append_event("a", "k", "{}").unwrap();
    store.append_event("a", "k", "{}").unwrap();
    store.append_event("b", "k", "{}").unwrap();

    let mut sessions = store.event_sessions().unwrap();
    sessions.sort();
    assert_eq!(sessions, vec!["a".to_string(), "b".to_string()]);
}

#[test]
fn event_sessions_is_empty_when_nothing_is_logged() {
    let store = Store::open_in_memory().unwrap();
    store.set("ns", "k", "v").unwrap();
    assert_eq!(
        store.event_sessions().unwrap(),
        [] as [std::string::String; 0]
    );
}

#[test]
fn a_fork_copies_a_prefix_and_renumbers_from_one() {
    let store = Store::open_in_memory().unwrap();
    for i in 1..=5 {
        store
            .append_event("parent", "k", &format!("{{\"i\":{i}}}"))
            .unwrap();
    }

    let copied = store.fork_events("parent", 3, "child").unwrap();
    assert_eq!(copied, 3, "only the prefix is copied");

    let child = store.session_events("child").unwrap();
    assert_eq!(
        child.iter().map(|e| e.seq).collect::<Vec<_>>(),
        vec![1, 2, 3],
        "the fork's own log reads as a sequence"
    );
    assert_eq!(
        child.iter().map(|e| e.payload.clone()).collect::<Vec<_>>(),
        store.session_events("parent").unwrap()[..3]
            .iter()
            .map(|e| e.payload.clone())
            .collect::<Vec<_>>(),
        "and carries the same payloads, in the same order"
    );
    assert!(
        child.iter().all(|e| e.session == "child"),
        "attributed to the fork, not the parent"
    );
}

/// Appends to either side after a fork stay out of the other.
#[test]
fn a_fork_and_its_parent_diverge() {
    let store = Store::open_in_memory().unwrap();
    store.append_event("parent", "k", "{\"i\":1}").unwrap();
    store.append_event("parent", "k", "{\"i\":2}").unwrap();
    store.fork_events("parent", 1, "child").unwrap();

    store
        .append_event("child", "k", "{\"only\":\"child\"}")
        .unwrap();
    store
        .append_event("parent", "k", "{\"only\":\"parent\"}")
        .unwrap();

    let child = store.session_events("child").unwrap();
    let parent = store.session_events("parent").unwrap();
    assert_eq!(child.len(), 2, "prefix + its own new event");
    assert_eq!(parent.len(), 3, "untouched by the fork, then its own");
    assert!(
        child.iter().all(|e| !e.payload.contains("only\":\"parent")),
        "the parent's later events did not reach the fork"
    );
    assert!(
        parent.iter().all(|e| !e.payload.contains("only\":\"child")),
        "nor the fork's the parent"
    );
    assert_eq!(
        child.last().unwrap().seq,
        2,
        "the fork's numbering continues from its own prefix, not the parent's"
    );
}

/// Forking into a session that already has a log would interleave two histories; refused.
#[test]
fn forking_into_a_used_session_is_refused() {
    let store = Store::open_in_memory().unwrap();
    store.append_event("parent", "k", "{}").unwrap();
    store.append_event("taken", "k", "{}").unwrap();

    let Err(StoreError::Backend { detail }) = store.fork_events("parent", 1, "taken") else {
        panic!("expected a refusal");
    };
    assert!(
        detail.contains("taken") && detail.contains("already has events"),
        "the refusal names the session and why: {detail}"
    );
    assert_eq!(
        store.session_events("taken").unwrap().len(),
        1,
        "and nothing was copied"
    );
}

/// A fork keeps the parent's timestamps; restamping would claim they happened at fork time.
#[test]
fn a_fork_preserves_the_original_timestamps() {
    let store = Store::open_in_memory().unwrap();
    let original = store.append_event("parent", "k", "{}").unwrap();
    store.fork_events("parent", 1, "child").unwrap();
    assert_eq!(store.session_events("child").unwrap()[0].ts, original.ts);
}

/// Forking past the end copies what exists; forking at 0 copies nothing. The caller decides what counts as an error.
#[test]
fn a_fork_bound_outside_the_log_copies_what_there_is() {
    let store = Store::open_in_memory().unwrap();
    store.append_event("parent", "k", "{}").unwrap();
    store.append_event("parent", "k", "{}").unwrap();

    assert_eq!(store.fork_events("parent", 99, "all").unwrap(), 2);
    assert_eq!(store.fork_events("parent", 0, "none").unwrap(), 0);
    assert_eq!(
        store.session_events("none").unwrap(),
        [] as [jk_session::store::LoggedEvent; 0]
    );
}

/// Appends one turn: a `user-message`, `tool_calls` `tool-invoked`/`tool-result` pairs, then a `done`.
/// Row counts vary per turn on purpose, which defeats a row-based `LIMIT`.
fn append_turn(store: &Store, session: &str, turn: usize, tool_calls: usize) {
    store
        .append_event(
            session,
            KIND_USER_MESSAGE,
            &envelope(&serde_json::json!({ "message": format!("q{turn}") })),
        )
        .unwrap();
    for call in 0..tool_calls {
        let id = format!("t{turn}-{call}");
        let (kind, payload) = encode(&Event::ToolInvoked(ToolCall {
            id: id.clone(),
            name: "tool".to_owned(),
            arguments: "{}".to_owned(),
        }));
        store.append_event(session, kind, &payload).unwrap();
        let (kind, payload) = encode(&Event::ToolResult(ToolOutcome {
            tool_call_id: id,
            content: format!("result{turn}-{call}"),
            failed: false,
        }));
        store.append_event(session, kind, &payload).unwrap();
    }
    let (kind, payload) = encode(&Event::Done {
        text: format!("a{turn}"),
        agentic: tool_calls > 0,
    });
    store.append_event(session, kind, &payload).unwrap();
}

/// Builds `session`'s log with one turn per entry of `tool_calls_per_turn`, each making that many tool calls.
fn build_session(store: &Store, session: &str, tool_calls_per_turn: &[usize]) {
    for (turn, &tool_calls) in tool_calls_per_turn.iter().enumerate() {
        append_turn(store, session, turn, tool_calls);
    }
}

/// `recent_turns` (SQL) and `last_turns` (in memory) are independent; this asserts they agree,
/// so a bounded replay never diverges from the full projection for the same bound.
fn assert_recent_turns_matches_last_turns(store: &Store, session: &str, turns: u32) {
    let whole = store.session_events(session).unwrap();
    let expected = projection::last_turns(&whole, turns).to_vec();
    let actual = store.recent_turns(session, turns).unwrap();
    assert_eq!(
        actual,
        expected,
        "session={session:?} turns={turns} whole_log_len={} disagree",
        whole.len()
    );
}

#[test]
fn recent_turns_matches_last_turns_when_the_session_has_fewer_turns_than_the_bound() {
    let store = Store::open_in_memory().unwrap();
    build_session(&store, "s", &[0, 1, 2]);
    for turns in [0, 1, 3, 4, 20] {
        assert_recent_turns_matches_last_turns(&store, "s", turns);
    }
}

#[test]
fn recent_turns_matches_last_turns_at_exactly_the_bound() {
    let store = Store::open_in_memory().unwrap();
    build_session(&store, "s", &[1; 20]);
    assert_recent_turns_matches_last_turns(&store, "s", 20);
    assert_recent_turns_matches_last_turns(&store, "s", 19);
    assert_recent_turns_matches_last_turns(&store, "s", 21);
}

/// Many turns with varying tool-call counts (0, 1, 2, 3, repeating): the boundary turn's row count differs from its neighbours.
#[test]
fn recent_turns_matches_last_turns_on_a_long_session_with_variable_row_counts() {
    let store = Store::open_in_memory().unwrap();
    let tool_calls_per_turn: Vec<usize> = (0..200).map(|i| i % 4).collect();
    build_session(&store, "s", &tool_calls_per_turn);
    for turns in [0, 1, 20, 199, 200, 201] {
        assert_recent_turns_matches_last_turns(&store, "s", turns);
    }
}

#[test]
fn recent_turns_matches_last_turns_on_an_empty_session() {
    let store = Store::open_in_memory().unwrap();
    for turns in [0, 1, 20] {
        assert_recent_turns_matches_last_turns(&store, "never-used", turns);
    }
}

/// Seeded xorshift64 sweep over generated logs, standing in for a property test (no proptest dependency).
/// Covers bounds aligned to turn boundaries, bounds larger than the log, and mixed zero/multi-call turns.
#[test]
fn recent_turns_agrees_with_last_turns_across_many_generated_logs() {
    struct Xorshift64(u64);
    impl Xorshift64 {
        fn next_u32(&mut self) -> u32 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            (x >> 32) as u32
        }
        fn below(&mut self, bound: u32) -> u32 {
            self.next_u32() % bound
        }
    }

    for seed in 1..=8_u64 {
        let mut rng = Xorshift64(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let turn_count: u32 = rng.below(60);
        let tool_calls_per_turn: Vec<usize> =
            (0..turn_count).map(|_| rng.below(4) as usize).collect();

        let store = Store::open_in_memory().unwrap();
        let session = format!("seed-{seed}");
        build_session(&store, &session, &tool_calls_per_turn);

        for turns in [0, 1, 2, turn_count, turn_count + 1, 200] {
            assert_recent_turns_matches_last_turns(&store, &session, turns);
        }
        for _ in 0..5 {
            let bound = if turn_count == 0 {
                0
            } else {
                rng.below(turn_count + 1)
            };
            assert_recent_turns_matches_last_turns(&store, &session, bound);
        }
    }
}

/// On a long session `recent_turns` reads a bounded number of rows while `session_events` reads the whole log;
/// both must resolve to the same conversation.
#[test]
fn recent_turns_reads_a_small_bounded_tail_of_a_long_session_with_the_same_messages() {
    let store = Store::open_in_memory().unwrap();
    let tool_calls_per_turn: Vec<usize> = (0..1000).map(|i| i % 3).collect();
    build_session(&store, "long", &tool_calls_per_turn);

    let turns = 20;
    let whole = store.session_events("long").unwrap();
    let bounded = store.recent_turns("long", turns).unwrap();

    assert!(
        bounded.len() < whole.len() / 20,
        "bounded read ({} rows) should be a small fraction of the whole log \
         ({} rows) on a 1000-turn session",
        bounded.len(),
        whole.len()
    );

    let as_tuples = |messages: Vec<jk_protocol::turn::Message>| {
        messages
            .into_iter()
            .map(|m| (m.role, m.content, m.tool_call_id))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        as_tuples(projection::transcript(&bounded)),
        as_tuples(projection::transcript(projection::last_turns(
            &whole, turns
        ))),
        "same messages whether read bounded from SQL or trimmed in memory \
         from the whole log"
    );
}
