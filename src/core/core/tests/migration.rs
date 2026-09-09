//! The upgrade path: a database written before the event log existed becomes
//! readable again.
//!
//! `event_log`'s own unit tests cover the conversion. What this asserts is the
//! part they cannot — that the **boot path runs it**, so an operator upgrading
//! does not have to know it exists. Acceptance line 3 of #45 removed the
//! `entries` transcript write, and without this those sessions are neither
//! listed nor readable while their rows sit there unread.
//!
//! The fixture is built with `rusqlite` directly, so it is the old schema
//! holding the old rows rather than a simulation of them. No extensions are
//! enabled: this is about the store, and a provider guest would only add a
//! reason for the test to skip.

use jan_klod_core::http::WireError;
use jan_klod_core::route::HttpFn;
use jan_klod_core::store::Store;
use jan_klod_core::Runtime;

/// A database as an install from before the log would have it: the `entries`
/// table alone, holding two turns of one session, with their own timestamps.
fn old_database(path: &std::path::Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE entries (
            namespace  TEXT    NOT NULL,
            key        TEXT    NOT NULL,
            value      TEXT    NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY (namespace, key)
        );
        INSERT INTO entries VALUES
          ('history', 'turn-1',
           '{\"user\":\"what did we decide?\",\"answer\":\"to ship it\"}', 111, 111),
          ('history', 'turn-2',
           '{\"user\":\"and then?\",\"answer\":\"to write it down\"}', 222, 222);",
    )
    .unwrap();
}

#[test]
fn a_database_from_before_the_log_is_readable_after_a_boot() {
    let dir = std::env::temp_dir().join(format!(
        "jk-migrate-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let db = dir.join("jan-klod.db");
    old_database(&db);

    let config = dir.join("config.yaml");
    std::fs::write(&config, format!("\nstorage:\n  path: {}\n", db.display())).unwrap();

    let runtime = Runtime::boot(&config, dir.join("ext")).expect("runtime boots");
    // No provider is enabled, so this is never called; it exists because
    // `build_agent` takes a factory.
    let factory = || -> HttpFn { Box::new(|_m, _u, _h, _b, _t| Err(WireError::ConnectionFailed)) };
    let agent = runtime.build_agent(&factory).expect("agent boots");

    assert_eq!(
        agent.list_sessions(),
        vec!["history".to_string()],
        "the old session is listed again — boot converted it without being asked"
    );
    assert_eq!(
        agent
            .transcript("history")
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>(),
        vec![
            "what did we decide?",
            "to ship it",
            "and then?",
            "to write it down"
        ],
        "and reads as the conversation it was, in turn order"
    );

    let store = Store::open(&db).expect("the database is still readable directly");
    let log = store.session_events("history").expect("it has a log now");
    assert_eq!(log.len(), 4, "two turns, two rows each");
    assert!(
        log.iter().all(|row| row.ts == 111 || row.ts == 222),
        "dated when the turns happened, not when the upgrade ran: {:?}",
        log.iter().map(|r| r.ts).collect::<Vec<_>>()
    );

    // The old rows are left in place. Deleting them would make the migration
    // unrepeatable and irreversible in the same step, and they cost nothing.
    assert!(
        store.get("history", "turn-1").is_ok(),
        "the transcript rows are not destroyed by the conversion"
    );

    std::fs::remove_dir_all(&dir).ok();
}
