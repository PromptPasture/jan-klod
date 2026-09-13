//! SQLite-backed key/value + history store. Implements full
//! [`memory-store`](../../../wit/memory-store.wit) (superset of `host-storage`).
//! Host-side by design: sandbox gets no filesystem, database in core, served
//! to extensions. SQLite embedded via `rusqlite`'s bundled feature.
//!
//! Two tables: `entries` (key/value on `namespace, key`; opaque JSON values),
//! `events` (append-only turn log on `session, seq`). Timestamps in Unix seconds.

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
    /// Grouped (not bare `DISTINCT` + `ORDER BY MAX(...)`, SQLite rejects that).
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
    /// `seq` allocated in-statement (`SELECT COALESCE(MAX(seq), 0) + 1 RETURNING seq`)
    /// so no other append can claim that maximum. Core serialises callers via
    /// `Arc<Mutex<Store>>` anyway. No update/delete: the log records what happened;
    /// an API that rewrites it makes every replay a claim about the present.
    /// [`Self::purge_session_events`] is the sole exception for forgetting sessions.
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

    /// Append one event with an explicit timestamp (for migrations). Stamping
    /// old transcripts with migration time would date them to the upgrade, which
    /// the log must never invent.
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

    /// Every event logged for `session`, in order. Unbounded on purpose: callers
    /// (projection, fork, tests) want the whole log. [`Self::recent_turns`] is
    /// the bounded sibling for `replay`, which wants only the end.
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

    /// Tail of `session`'s log with at most `turns` turns. Bounded sibling of
    /// [`Self::session_events`] for `replay` to avoid reading long sessions.
    ///
    /// A turn begins at `user-message` and spans multiple rows (tool calls add two).
    /// A plain `LIMIT` would cut turns in half; instead this finds the earliest
    /// `user-message` seq among the last `turns` messages and reads from there.
    /// ([`crate::projection::last_turns`] has the in-memory rule this must match.)
    ///
    /// `COALESCE(MIN(seq), 0)` handles sessions with fewer than `turns` messages:
    /// the inner query returns fewer rows or zero, `MIN` is the earliest or `NULL`,
    /// and `COALESCE` turns `NULL` to `0` (seq no row can be below), matching
    /// `last_turns` returning the whole slice when there are not that many turns.
    ///
    /// `turns == 0` exits before the query runs (not `LIMIT 0`): an empty inner
    /// query would still make `COALESCE` fall back to `0` and read the whole log.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure.
    pub fn recent_turns(&self, session: &str, turns: u32) -> Result<Vec<LoggedEvent>, StoreError> {
        if turns == 0 {
            return Ok(Vec::new());
        }
        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, ts, kind, payload FROM events
                 WHERE session = ?1 AND seq >= (
                     SELECT COALESCE(MIN(seq), 0) FROM (
                         SELECT seq FROM events
                         WHERE session = ?1 AND kind = ?3
                         ORDER BY seq DESC LIMIT ?2
                     )
                 )
                 ORDER BY seq",
            )
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        let rows = stmt
            .query_map(
                params![session, turns, crate::event_log::KIND_USER_MESSAGE],
                |row| {
                    Ok(LoggedEvent {
                        session: session.to_string(),
                        seq: to_u64(row.get::<_, i64>(0)?),
                        ts: to_u64(row.get::<_, i64>(1)?),
                        kind: row.get::<_, String>(2)?,
                        payload: row.get::<_, String>(3)?,
                    })
                },
            )
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })
    }

    /// Every session with logged events, most recent first. Event-log counterpart
    /// to [`Self::list_namespaces`], needed separately: namespaces exist in
    /// `entries` while transcripts are written there; `list_namespaces` reports
    /// nothing once they stop, while the log persists. Both ordered by `MAX(ts)`
    /// DESC then id to agree while both exist.
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

    /// Copy `session`'s events up to `at_seq` into `into`, renumbered from 1.
    /// Returns how many rows were copied.
    ///
    /// Original `ts` is preserved (these are the same facts; stamping them with
    /// fork's creation time would claim they happened then). `seq` is assigned by
    /// `ROW_NUMBER()` not carried, so fork's log is `1..n` by construction; today
    /// that's the identity (appends contiguous), but this keeps it true always.
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
