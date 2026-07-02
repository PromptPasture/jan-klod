//! Host-side persistent store (Phase 3 Slice 3a).
//!
//! A SQLite-backed key/value + history store implementing the full
//! [`memory-store`](../../../wit/memory-store.wit) operation set (a superset of
//! `host-storage`). Persistence is **host-side** by design: the sandbox grants no
//! filesystem, so the database lives in core and is served to extensions through
//! the storage contracts. `SQLite` is embedded via `rusqlite`'s `bundled` feature,
//! so there is no system-library dependency.
//!
//! The schema is one table keyed by `(namespace, key)`; values are opaque JSON
//! strings (the core never interprets them). Timestamps are Unix seconds.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

/// A stored entry. Mirrors `store-types.entry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `<namespace>/<key>` — the row's stable id.
    pub id: String,
    /// Grouping namespace.
    pub namespace: String,
    /// Key within the namespace.
    pub key: String,
    /// Opaque JSON-encoded value (empty on `list-keys`, which omits the payload).
    pub value: String,
    /// Unix timestamp (seconds) of first insert.
    pub created_at: u64,
    /// Unix timestamp (seconds) of the last update.
    pub updated_at: u64,
}

/// Errors the store can surface. Mirrors `store-types.store-error`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// No entry for the requested namespace + key.
    #[error("entry not found")]
    NotFound,
    /// Any other backend failure (I/O, SQL, …).
    #[error("storage backend error: {detail}")]
    Backend {
        /// The underlying SQL or I/O error message.
        detail: String,
    },
}

/// A SQLite-backed store. One [`Connection`]; the core owns a single instance and
/// brokers `host-storage` calls into it.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Open (creating if needed) a store at `path`, ensuring the schema exists.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the database cannot be opened or the
    /// schema cannot be created.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path)
            .map_err(|e| StoreError::Backend { detail: e.to_string() })?;
        Self::init(conn)
    }

    /// Open an ephemeral in-memory store (tests, transient state).
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the connection cannot be created.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory()
            .map_err(|e| StoreError::Backend { detail: e.to_string() })?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<Self, StoreError> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS entries (
                namespace  TEXT    NOT NULL,
                key        TEXT    NOT NULL,
                value      TEXT    NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY (namespace, key)
            );",
        )
        .map_err(|e| StoreError::Backend { detail: e.to_string() })?;
        Ok(Self { conn })
    }

    /// Upsert `value` at `(namespace, key)`, returning the stored entry.
    /// `created_at` is preserved across updates; `updated_at` advances.
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] (with the SQL error message) on a SQL failure,
    /// or [`StoreError::NotFound`] if the row cannot be read back (should not happen
    /// after a successful write).
    pub fn set(&self, namespace: &str, key: &str, value: &str) -> Result<Entry, StoreError> {
        let now = now_secs();
        self.conn
            .execute(
                "INSERT INTO entries (namespace, key, value, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?4)
                 ON CONFLICT(namespace, key)
                 DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                params![namespace, key, value, i64::try_from(now).unwrap_or(i64::MAX)],
            )
            .map_err(|e| StoreError::Backend { detail: e.to_string() })?;
        self.get(namespace, key)
    }

    /// Fetch the entry at `(namespace, key)`.
    ///
    /// # Errors
    /// [`StoreError::NotFound`] if absent, [`StoreError::Backend`] on a SQL failure.
    pub fn get(&self, namespace: &str, key: &str) -> Result<Entry, StoreError> {
        self.conn
            .query_row(
                "SELECT value, created_at, updated_at FROM entries
                 WHERE namespace = ?1 AND key = ?2",
                params![namespace, key],
                |row| {
                    Ok(Entry {
                        id: format!("{namespace}/{key}"),
                        namespace: namespace.to_string(),
                        key: key.to_string(),
                        value: row.get::<_, String>(0)?,
                        created_at: to_u64(row.get::<_, i64>(1)?),
                        updated_at: to_u64(row.get::<_, i64>(2)?),
                    })
                },
            )
            .optional()
            .map_err(|e| StoreError::Backend { detail: e.to_string() })?
            .ok_or(StoreError::NotFound)
    }

    /// Delete `(namespace, key)`. A no-op if absent (per the contract).
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn delete(&self, namespace: &str, key: &str) -> Result<(), StoreError> {
        self.conn
            .execute(
                "DELETE FROM entries WHERE namespace = ?1 AND key = ?2",
                params![namespace, key],
            )
            .map(|_| ())
            .map_err(|e| StoreError::Backend { detail: e.to_string() })
    }

    /// List every key in `namespace`, newest-first, **without** the value payload.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn list_keys(&self, namespace: &str) -> Result<Vec<Entry>, StoreError> {
        self.query_entries(
            "SELECT key, '', created_at, updated_at FROM entries
             WHERE namespace = ?1 ORDER BY updated_at DESC, rowid DESC",
            params![namespace],
            namespace,
        )
    }

    /// The `limit` most recent entries in `namespace`, newest-first.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn recent(&self, namespace: &str, limit: u32) -> Result<Vec<Entry>, StoreError> {
        self.query_entries(
            "SELECT key, value, created_at, updated_at FROM entries
             WHERE namespace = ?1 ORDER BY updated_at DESC, rowid DESC LIMIT ?2",
            params![namespace, limit],
            namespace,
        )
    }

    /// Substring search over values in `namespace`, newest-first (best-effort).
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn search(&self, namespace: &str, query: &str, limit: u32) -> Result<Vec<Entry>, StoreError> {
        let pattern = format!("%{query}%");
        self.query_entries(
            "SELECT key, value, created_at, updated_at FROM entries
             WHERE namespace = ?1 AND value LIKE ?2 ORDER BY updated_at DESC, rowid DESC LIMIT ?3",
            params![namespace, pattern, limit],
            namespace,
        )
    }

    /// Delete every entry in `namespace`.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn purge_namespace(&self, namespace: &str) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM entries WHERE namespace = ?1", params![namespace])
            .map(|_| ())
            .map_err(|e| StoreError::Backend { detail: e.to_string() })
    }

    /// Run a `SELECT key, value, created_at, updated_at` query into `Entry`s.
    fn query_entries(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        namespace: &str,
    ) -> Result<Vec<Entry>, StoreError> {
        let mut stmt = self.conn.prepare(sql).map_err(|e| StoreError::Backend { detail: e.to_string() })?;
        let rows = stmt
            .query_map(params, |row| {
                let key: String = row.get(0)?;
                Ok(Entry {
                    id: format!("{namespace}/{key}"),
                    namespace: namespace.to_string(),
                    key,
                    value: row.get::<_, String>(1)?,
                    created_at: to_u64(row.get::<_, i64>(2)?),
                    updated_at: to_u64(row.get::<_, i64>(3)?),
                })
            })
            .map_err(|e| StoreError::Backend { detail: e.to_string() })?;
        rows.collect::<Result<Vec<_>, _>>().map_err(|e| StoreError::Backend { detail: e.to_string() })
    }
}

/// Clamp a `SQLite` `INTEGER` timestamp to `u64` (stored values are non-negative).
fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// Current Unix time in seconds (saturating at 0 before the epoch).
fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        let path = std::env::temp_dir().join(format!("jk-store-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        {
            let store = Store::open(&path).unwrap();
            store.set("session", "history", "durable").unwrap();
        } // store dropped, connection closed
        {
            let store = Store::open(&path).unwrap();
            assert_eq!(store.get("session", "history").unwrap().value, "durable");
        }
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn backend_error_carries_detail() {
        // A path whose parent directory does not exist cannot be opened.
        let result = Store::open("/nonexistent/deep/path/cannot/exist/db.sqlite");
        let Err(StoreError::Backend { detail }) = result else {
            panic!("expected Err(Backend), got Ok or a different error variant");
        };
        assert!(!detail.is_empty(), "detail must describe the failure, got empty string");
    }
}
