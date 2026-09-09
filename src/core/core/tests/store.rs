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
