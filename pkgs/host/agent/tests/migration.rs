//! Boot converts a pre-event-log database, so an upgraded install stays readable; the `event_log` unit tests cover the
//! conversion itself, this covers that boot runs it. No extensions are enabled, since a provider guest would only add a skip reason.

use jk_agent::Runtime;
use jk_session::store::Store;
use jk_wasm::http::WireError;
use jk_wasm::route::HttpFn;

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

    assert!(
        store.get("history", "turn-1").is_ok(),
        "the transcript rows are not destroyed by the conversion"
    );

    std::fs::remove_dir_all(&dir).ok();
}
