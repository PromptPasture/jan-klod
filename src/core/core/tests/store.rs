//! The host-side persistent store.

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
