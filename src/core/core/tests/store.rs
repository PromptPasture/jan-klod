//! The host-side persistent store.
//!
//! These files sat in `src/core/tests/` from the move that split the repo into
//! `src/core` + `src/extensions`. That directory is beside a **virtual** workspace
//! manifest (`[workspace]`, no `[package]`), so it belongs to no crate and cargo
//! never built it: eighteen tests across three files, silently not running for as
//! long as they lived there. `cargo test` reported success the whole time, because
//! a test that is not compiled cannot fail.
//!
//! All eighteen passed once compiled — the loss was not a hidden failure here but
//! eighteen assertions that were not defending anything. What *was* broken sat one
//! function away: `list_namespaces` had no test at all, and had never once executed
//! its own query successfully.

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
    assert_eq!(second.created_at, first.created_at, "created_at is stable across updates");
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
    assert!(keys.iter().all(|e| e.value.is_empty()), "list-keys omits the payload");

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
    assert!(!detail.is_empty(), "detail must describe the failure, got empty string");
}

/// `list_namespaces` was never called by a test, and its one caller swallowed
/// errors — so a query that failed on every invocation looked like an empty
/// database for as long as nobody looked.
#[test]
fn namespaces_list_most_recently_written_first() {
    let store = Store::open_in_memory().unwrap();
    store.set("alpha", "k", "1").unwrap();
    store.set("beta", "k", "1").unwrap();
    store.set("alpha", "k2", "2").unwrap();

    let namespaces = store.list_namespaces().expect("the query is valid SQL");
    assert_eq!(namespaces.len(), 2, "one row per namespace, not per entry: {namespaces:?}");
    assert!(namespaces.contains(&"alpha".to_string()));
    assert!(namespaces.contains(&"beta".to_string()));
}
