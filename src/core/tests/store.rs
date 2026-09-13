//! The host-side persistent store.

use jan_klod_core::conductor::Event;
use jan_klod_core::event_log::{encode, envelope, KIND_USER_MESSAGE};
use jan_klod_core::intercept::{ToolCall, ToolOutcome};
use jan_klod_core::projection;
use jan_klod_core::store::{Store, StoreError};

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

// ─── The append-only turn log ────────────────────────────────────────────────

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

/// Each session numbers its own log. Sharing a counter would make `seq` a
/// global clock, and then a session's log could not be read as a sequence
/// without gaps — which is exactly what a replay needs it to be.
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

/// Ordering has to hold past the point where string comparison stops agreeing
/// with numeric order — `"10" < "9"` — so this appends into double digits.
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

/// The log survives a reopen, and — the part worth asserting — an existing
/// database file created before this table existed gains it on open, because
/// the schema is `CREATE TABLE IF NOT EXISTS`. Without that, every deployed
/// `jan-klod.db` would fail on the first append.
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

    // Build a database whose schema predates `events` — the `entries` table
    // alone, exactly as a deployed `jan-klod.db` has it — using rusqlite
    // directly rather than adding a test-only method to `Store`.
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

// ─── What a projection needs from the store ──────────────────────────────────

/// The reason `event_sessions` exists rather than reusing `list_namespaces`:
/// a session can have a log and no entries at all, which is exactly the state
/// every session is in once the transcript stops being written to `entries`.
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
    assert!(store.event_sessions().unwrap().is_empty());
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

/// A fork is independent in both directions: appending to either afterwards
/// must not appear in the other.
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

/// Forking into a session that already has a log would interleave two
/// histories, and the result would read as one. Refused with a reason.
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

/// A fork keeps the original timestamps: these are the parent's facts, and
/// restamping them would claim they happened when the fork was made.
#[test]
fn a_fork_preserves_the_original_timestamps() {
    let store = Store::open_in_memory().unwrap();
    let original = store.append_event("parent", "k", "{}").unwrap();
    store.fork_events("parent", 1, "child").unwrap();
    assert_eq!(store.session_events("child").unwrap()[0].ts, original.ts);
}

/// Forking past the end copies what exists; forking at 0 copies nothing. Both
/// are the caller's decision to make, so the store reports the count rather
/// than guessing which is an error.
#[test]
fn a_fork_bound_outside_the_log_copies_what_there_is() {
    let store = Store::open_in_memory().unwrap();
    store.append_event("parent", "k", "{}").unwrap();
    store.append_event("parent", "k", "{}").unwrap();

    assert_eq!(store.fork_events("parent", 99, "all").unwrap(), 2);
    assert_eq!(store.fork_events("parent", 0, "none").unwrap(), 0);
    assert!(store.session_events("none").unwrap().is_empty());
}

// ─── `recent_turns`: the SQL bound, checked against the in-memory rule (#85) ─

/// Appends one turn to `session`: a `user-message`, then `tool_calls` pairs of
/// `tool-invoked`/`tool-result` — the variable-row-count case a plain `LIMIT`
/// cannot express — then a `done`. Turns in the same session vary in row
/// count on purpose: 0 tool calls is 2 rows, 1 is 4, and so on, which is
/// exactly the shape that defeats a row-based bound.
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

/// Builds `session`'s log with one turn per entry of `tool_calls_per_turn`
/// (that entry's value is how many tool calls that turn makes).
fn build_session(store: &Store, session: &str, tool_calls_per_turn: &[usize]) {
    for (turn, &tool_calls) in tool_calls_per_turn.iter().enumerate() {
        append_turn(store, session, turn, tool_calls);
    }
}

/// The two independent paths to a bounded replay must agree: `recent_turns`
/// is a SQL query over `events`, `last_turns` is a slice over the whole log
/// held in memory, and neither calls the other. If they disagreed, a replay
/// would silently hand the model a different history than a full projection
/// (`GET /session/:id`) would show for the same bound — the exact drift this
/// asserts cannot happen.
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
    // One turn less than the log has, and one more: both sides of the exact
    // match, still agreeing.
    assert_recent_turns_matches_last_turns(&store, "s", 19);
    assert_recent_turns_matches_last_turns(&store, "s", 21);
}

/// Many more turns than the bound, with a per-turn tool-call count that
/// varies (0, 1, 2, 3, repeating) — the case a `LIMIT` cannot express,
/// because the turn kept at the boundary is a different number of rows than
/// its neighbours.
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

/// A deterministic sweep over many generated logs, standing in for a
/// property test: the repo has no `proptest`/`quickcheck` dependency, so this
/// is a small seeded PRNG (xorshift64, `std` only) driving many
/// turn-count/tool-call-count/bound combinations instead. Reproducible across
/// runs, and — the point of it — exercises shapes a handful of hand-picked
/// literals would not: bounds that land exactly on a turn boundary with a
/// preceding turn of a different row count, bounds larger than the log,
/// zero-tool-call turns next to multi-tool-call ones, and so on.
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
        // A handful of bounds strictly between 0 and the log's own turn
        // count, landing mid-log rather than only at its edges.
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

/// The measurable win #85 asks for: on a session long enough that trimming
/// in SQL versus in memory is not a rounding error, `recent_turns` reads a
/// small, bounded number of rows while `session_events` reads (and this
/// asserts, decodes into) the whole log — and the two still resolve to the
/// same conversation.
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

    // `Message` has no `PartialEq` (it is not a comparable value anywhere
    // else in the crate), so the comparison is over `(role, content,
    // tool_call_id)` tuples rather than the messages themselves.
    let as_tuples = |messages: Vec<jan_klod_core::intercept::Message>| {
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
