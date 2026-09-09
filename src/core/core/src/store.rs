//! Host-side persistent store.
//!
//! A SQLite-backed key/value + history store implementing the full
//! [`memory-store`](../../../wit/memory-store.wit) operation set (a superset of
//! `host-storage`). Persistence is **host-side** by design: the sandbox grants no
//! filesystem, so the database lives in core and is served to extensions through
//! the storage contracts. `SQLite` is embedded via `rusqlite`'s `bundled` feature,
//! so there is no system-library dependency.
//!
//! Two tables. `entries` is the key/value store, keyed by `(namespace, key)`;
//! values are opaque JSON strings (the core never interprets them). `events` is
//! the append-only turn log, keyed by `(session, seq)` — see
//! [`Store::append_event`]. Timestamps are Unix seconds.

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

/// One row of a session's append-only turn log.
///
/// Not an [`Entry`]: an entry is current state addressed by a key and updated in
/// place, while this is a fact addressed by its position and never changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedEvent {
    /// The session whose log this belongs to.
    pub session: String,
    /// Position in that log, starting at 1 and increasing by one per append.
    pub seq: u64,
    /// Unix timestamp (seconds) of the append.
    pub ts: u64,
    /// Which kind of event this is — the payload's discriminator.
    pub kind: String,
    /// Opaque JSON payload. The store never interprets it; the envelope and its
    /// version are the caller's business.
    pub payload: String,
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
        let conn = Connection::open(path).map_err(|e| StoreError::Backend {
            detail: e.to_string(),
        })?;
        Self::init(conn)
    }

    /// Open an ephemeral in-memory store (tests, transient state).
    ///
    /// # Errors
    /// Returns [`StoreError::Backend`] if the connection cannot be created.
    pub fn open_in_memory() -> Result<Self, StoreError> {
        let conn = Connection::open_in_memory().map_err(|e| StoreError::Backend {
            detail: e.to_string(),
        })?;
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
            );
            CREATE TABLE IF NOT EXISTS events (
                session TEXT    NOT NULL,
                seq     INTEGER NOT NULL,
                ts      INTEGER NOT NULL,
                kind    TEXT    NOT NULL,
                payload TEXT    NOT NULL,
                PRIMARY KEY (session, seq)
            );",
        )
        .map_err(|e| StoreError::Backend {
            detail: e.to_string(),
        })?;
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
                params![
                    namespace,
                    key,
                    value,
                    i64::try_from(now).unwrap_or(i64::MAX)
                ],
            )
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
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
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?
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
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })
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
    pub fn search(
        &self,
        namespace: &str,
        query: &str,
        limit: u32,
    ) -> Result<Vec<Entry>, StoreError> {
        let pattern = format!("%{query}%");
        self.query_entries(
            "SELECT key, value, created_at, updated_at FROM entries
             WHERE namespace = ?1 AND value LIKE ?2 ORDER BY updated_at DESC, rowid DESC LIMIT ?3",
            params![namespace, pattern, limit],
            namespace,
        )
    }

    /// List every distinct namespace, most recently written first.
    ///
    /// Grouped rather than a bare `DISTINCT` + `ORDER BY MAX(...)`, which `SQLite`
    /// rejects as a misuse of aggregates.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn list_namespaces(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT namespace FROM entries GROUP BY namespace \
                 ORDER BY MAX(updated_at) DESC, namespace ASC",
            )
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })
    }

    /// Delete every entry in `namespace`.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn purge_namespace(&self, namespace: &str) -> Result<(), StoreError> {
        self.conn
            .execute(
                "DELETE FROM entries WHERE namespace = ?1",
                params![namespace],
            )
            .map(|_| ())
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })
    }

    // ─── The append-only turn log ────────────────────────────────────────────

    /// Append one event to `session`'s log, returning the row.
    ///
    /// `seq` is allocated inside the same statement as the insert
    /// (`SELECT COALESCE(MAX(seq), 0) + 1 … RETURNING seq`), so it cannot read a
    /// maximum that another append has already claimed. The core serialises
    /// callers through `Arc<Mutex<Store>>` anyway, so this is not what stands
    /// between the log and a duplicate key today — it is that the alternative,
    /// reading the maximum and then inserting, would need a transaction wrapped
    /// around it to be equally safe and would still be two round trips.
    ///
    /// There is deliberately no update and no single-row delete: the log's value
    /// is that it records what happened, and an API that could rewrite it would
    /// make every replay a claim about the present rather than the past.
    /// [`Self::purge_session_events`] is the one exception, for forgetting a
    /// whole session.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn append_event(
        &self,
        session: &str,
        kind: &str,
        payload: &str,
    ) -> Result<LoggedEvent, StoreError> {
        self.append_event_at(session, kind, payload, now_secs())
    }

    /// Append one event with an explicit timestamp.
    ///
    /// For records of things that happened earlier than now — a migration of a
    /// transcript written before the log existed. Stamping those with the
    /// migration's own time would date every old session to the upgrade, which
    /// is the one fact about them a log must not invent.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn append_event_at(
        &self,
        session: &str,
        kind: &str,
        payload: &str,
        ts: u64,
    ) -> Result<LoggedEvent, StoreError> {
        let seq: i64 = self
            .conn
            .query_row(
                "INSERT INTO events (session, seq, ts, kind, payload)
                 SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4
                 FROM events WHERE session = ?1
                 RETURNING seq",
                params![
                    session,
                    i64::try_from(ts).unwrap_or(i64::MAX),
                    kind,
                    payload
                ],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        Ok(LoggedEvent {
            session: session.to_string(),
            seq: to_u64(seq),
            ts,
            kind: kind.to_string(),
            payload: payload.to_string(),
        })
    }

    /// Every event logged for `session`, in the order it happened.
    ///
    /// Unbounded on purpose: the callers are a replay and a test, and both want
    /// the whole log. A `limit` would have to be a *tail* to be useful, and a
    /// tail of an event log is not a projection of anything.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn session_events(&self, session: &str) -> Result<Vec<LoggedEvent>, StoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, ts, kind, payload FROM events
                 WHERE session = ?1 ORDER BY seq",
            )
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        let rows = stmt
            .query_map(params![session], |row| {
                Ok(LoggedEvent {
                    session: session.to_string(),
                    seq: to_u64(row.get::<_, i64>(0)?),
                    ts: to_u64(row.get::<_, i64>(1)?),
                    kind: row.get::<_, String>(2)?,
                    payload: row.get::<_, String>(3)?,
                })
            })
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })
    }

    /// Every session that has at least one logged event, most recent first.
    ///
    /// The event-log counterpart to [`Self::list_namespaces`], and needed
    /// separately rather than derived from it: a namespace exists in `entries`
    /// because a transcript was written there, so once the transcript stops
    /// being written that way, `list_namespaces` reports nothing while the log
    /// is full of sessions. Ordered `MAX(ts) DESC` then by id, matching how
    /// namespaces are ordered, so the two agree while both exist.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn event_sessions(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT session FROM events GROUP BY session \
                 ORDER BY MAX(ts) DESC, session ASC",
            )
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })
    }

    /// Copy `session`'s events up to and including `at_seq` into `into`,
    /// renumbered from 1. Returns how many rows were copied.
    ///
    /// The copy keeps each event's original `ts`. These are the same facts as
    /// the parent's, and stamping them with the fork's creation time would
    /// claim they happened when the fork was made.
    ///
    /// `seq` is assigned by `ROW_NUMBER()` rather than carried over, so the
    /// fork's log is `1..n` by construction. Today that is the identity —
    /// appends are contiguous, so `[1, at_seq]` already is `1..at_seq` — but a
    /// fork whose log started at 4 would not be readable as a sequence, and
    /// this way it cannot happen for a reason that has to stay true.
    ///
    /// # Errors
    /// [`StoreError::Backend`] if `into` already has events (a fork must start
    /// from nothing, or the two histories interleave) or on a SQL failure.
    pub fn fork_events(&self, session: &str, at_seq: u64, into: &str) -> Result<u64, StoreError> {
        if !self.session_events(into)?.is_empty() {
            return Err(StoreError::Backend {
                detail: format!("`{into}` already has events; a fork needs an unused session"),
            });
        }
        let copied = self
            .conn
            .execute(
                "INSERT INTO events (session, seq, ts, kind, payload)
                 SELECT ?2, ROW_NUMBER() OVER (ORDER BY seq), ts, kind, payload
                 FROM events WHERE session = ?1 AND seq <= ?3",
                params![session, into, i64::try_from(at_seq).unwrap_or(i64::MAX)],
            )
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        Ok(copied as u64)
    }

    /// Delete every event logged for `session` — the log's only removal path,
    /// matching [`Self::purge_namespace`]'s shape for entries.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn purge_session_events(&self, session: &str) -> Result<(), StoreError> {
        self.conn
            .execute("DELETE FROM events WHERE session = ?1", params![session])
            .map(|_| ())
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })
    }

    /// Run a `SELECT key, value, created_at, updated_at` query into `Entry`s.
    fn query_entries(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        namespace: &str,
    ) -> Result<Vec<Entry>, StoreError> {
        let mut stmt = self.conn.prepare(sql).map_err(|e| StoreError::Backend {
            detail: e.to_string(),
        })?;
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
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })
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
