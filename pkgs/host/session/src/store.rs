//! SQLite-backed key/value store and append-only turn log, host-side only; timestamps are Unix seconds.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};
use sha2::Digest;

/// A stored entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `<namespace>/<key>`, the row's stable id.
    pub id: String,
    /// Grouping namespace.
    pub namespace: String,
    /// Key within the namespace.
    pub key: String,
    /// Opaque JSON; empty from `list_keys`, which omits payloads.
    pub value: String,
    /// Unix seconds of first insert.
    pub created_at: u64,
    /// Unix seconds of the last update.
    pub updated_at: u64,
}

/// One row of a session's append-only turn log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoggedEvent {
    /// The owning session.
    pub session: String,
    /// Position in the log, starting at 1.
    pub seq: u64,
    /// Unix seconds of the append.
    pub ts: u64,
    /// The payload's discriminator.
    pub kind: String,
    /// Opaque JSON; the store never interprets it.
    pub payload: String,
}

/// Errors the store can surface.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StoreError {
    /// No entry for the namespace and key.
    #[error("entry not found")]
    NotFound,
    /// Any other backend failure (I/O, SQL).
    #[error("storage backend error: {detail}")]
    Backend {
        /// The underlying error message.
        detail: String,
    },
}

/// SQLite-backed store over one connection.
pub struct Store {
    conn: Connection,
}

impl Store {
    /// Opens (creating if needed) a store at `path` and ensures the schema.
    ///
    /// # Errors
    /// [`StoreError::Backend`] if the database or schema cannot be created.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let conn = Connection::open(path).map_err(|e| StoreError::Backend {
            detail: e.to_string(),
        })?;
        Self::init(conn)
    }

    /// Opens an ephemeral in-memory store.
    ///
    /// # Errors
    /// [`StoreError::Backend`] if the connection cannot be created.
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
                session     TEXT    NOT NULL,
                seq         INTEGER NOT NULL,
                ts          INTEGER NOT NULL,
                kind        TEXT    NOT NULL,
                payload     TEXT    NOT NULL,
                prev_digest TEXT,
                PRIMARY KEY (session, seq)
            );",
        )
        .map_err(|e| StoreError::Backend {
            detail: e.to_string(),
        })?;

        let store = Self { conn };
        store.ensure_prev_digest_column()?;

        Ok(store)
    }

    /// Upserts `value`, keeping `created_at` and advancing `updated_at`.
    ///
    /// # Errors
    /// [`StoreError::Backend`] on a SQL failure, or `NotFound` if the row cannot be read back.
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

    /// Fetches the entry at `(namespace, key)`.
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

    /// Deletes `(namespace, key)`; a no-op if absent.
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

    /// Lists every key in `namespace`, newest first, without the value payload.
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

    /// The `limit` most recent entries in `namespace`, newest first.
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

    /// Substring search over values in `namespace`, newest first (best-effort).
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

    /// Lists every distinct namespace, most recently written first.
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

    /// Deletes every entry in `namespace`.
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

    /// Appends one event to `session`'s log; `seq` is allocated in-statement so appends cannot collide.
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

    /// Appends one event with an explicit timestamp, for migrations.
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
        let prev_digest: Option<String> = self
            .conn
            .query_row(
                "SELECT seq, ts, kind, payload FROM events \
                 WHERE session = ?1 ORDER BY seq DESC LIMIT 1",
                params![session],
                |row| {
                    let seq = to_u64(row.get::<_, i64>(0)?);
                    let prev_ts = to_u64(row.get::<_, i64>(1)?);
                    let prev_kind = row.get::<_, String>(2)?;
                    let prev_payload = row.get::<_, String>(3)?;
                    Ok(Self::compute_event_digest(
                        seq,
                        prev_ts,
                        &prev_kind,
                        &prev_payload,
                    ))
                },
            )
            .optional()
            .map_err(|e| StoreError::Backend {
                detail: e.to_string(),
            })?;

        let seq: i64 = self
            .conn
            .query_row(
                "INSERT INTO events (session, seq, ts, kind, payload, prev_digest)
                 SELECT ?1, COALESCE(MAX(seq), 0) + 1, ?2, ?3, ?4, ?5
                 FROM events WHERE session = ?1
                 RETURNING seq",
                params![
                    session,
                    i64::try_from(ts).unwrap_or(i64::MAX),
                    kind,
                    payload,
                    prev_digest
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

    /// Every event logged for `session`, in order; unbounded, unlike [`Self::recent_turns`].
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

    /// Tail of `session`'s log with at most `turns` turns; must match [`crate::projection::last_turns`].
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

    /// Every session with logged events, most recent first.
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

    /// Copies `session`'s events up to `at_seq` into `into`, renumbered from 1, keeping `ts`; returns the count.
    ///
    /// # Errors
    /// [`StoreError::Backend`] if `into` already has events, or on a SQL failure.
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

    /// Deletes every event logged for `session`; the log's only removal path.
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

    fn ensure_prev_digest_column(&self) -> Result<(), StoreError> {
        let has_column = {
            let mut stmt = self
                .conn
                .prepare("PRAGMA table_info(events)")
                .map_err(|e| StoreError::Backend {
                    detail: e.to_string(),
                })?;
            let rows: Vec<String> = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| StoreError::Backend {
                    detail: e.to_string(),
                })?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| StoreError::Backend {
                    detail: e.to_string(),
                })?;
            rows.iter().any(|name| name == "prev_digest")
        };

        if !has_column {
            self.conn
                .execute("ALTER TABLE events ADD COLUMN prev_digest TEXT", [])
                .map_err(|e| StoreError::Backend {
                    detail: e.to_string(),
                })?;

            self.migrate_digests()?;
        }

        Ok(())
    }

    fn compute_event_digest(seq: u64, ts: u64, kind: &str, payload: &str) -> String {
        let tuple = format!("{seq}/{ts}/{kind}/{payload}");
        let mut hasher = sha2::Sha256::new();
        hasher.update(tuple.as_bytes());
        format!("{:x}", hasher.finalize())
    }

    /// Idempotent backfill: each event stores its predecessor's digest; the first stores NULL.
    pub(crate) fn migrate_digests(&self) -> Result<u64, StoreError> {
        let mut migrated = 0;

        let sessions: Vec<String> = {
            let mut stmt = self
                .conn
                .prepare("SELECT DISTINCT session FROM events ORDER BY session")
                .map_err(|e| StoreError::Backend {
                    detail: e.to_string(),
                })?;
            let rows = stmt
                .query_map([], |row| row.get(0))
                .map_err(|e| StoreError::Backend {
                    detail: e.to_string(),
                })?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(|e| StoreError::Backend {
                    detail: e.to_string(),
                })?
        };

        for session in sessions {
            let events: Vec<(u64, u64, String, String)> = {
                let mut stmt = self
                    .conn
                    .prepare(
                        "SELECT seq, ts, kind, payload FROM events \
                         WHERE session = ?1 ORDER BY seq ASC",
                    )
                    .map_err(|e| StoreError::Backend {
                        detail: e.to_string(),
                    })?;
                let rows = stmt
                    .query_map(params![&session], |row| {
                        Ok((
                            to_u64(row.get::<_, i64>(0)?),
                            to_u64(row.get::<_, i64>(1)?),
                            row.get::<_, String>(2)?,
                            row.get::<_, String>(3)?,
                        ))
                    })
                    .map_err(|e| StoreError::Backend {
                        detail: e.to_string(),
                    })?;
                rows.collect::<Result<Vec<_>, _>>()
                    .map_err(|e| StoreError::Backend {
                        detail: e.to_string(),
                    })?
            };

            for (i, (seq, _ts, _kind, _payload)) in events.iter().enumerate() {
                if i == 0 {
                    continue;
                }

                let (prev_seq, prev_ts, prev_kind, prev_payload) = &events[i - 1];
                let prev_digest =
                    Self::compute_event_digest(*prev_seq, *prev_ts, prev_kind, prev_payload);

                self.conn
                    .execute(
                        "UPDATE events SET prev_digest = ?1 WHERE session = ?2 AND seq = ?3",
                        params![prev_digest, &session, seq],
                    )
                    .map_err(|e| StoreError::Backend {
                        detail: e.to_string(),
                    })?;
            }

            migrated += 1;
        }

        Ok(migrated)
    }

    pub(crate) fn verify_chain_internal(
        &self,
        session: &str,
    ) -> Result<crate::event_log::ChainVerification, crate::event_log::VerifyError> {
        use crate::event_log::{ChainStatus, ChainVerification, VerifyError};

        let mut stmt = self
            .conn
            .prepare(
                "SELECT seq, ts, kind, payload, prev_digest FROM events \
                 WHERE session = ?1 ORDER BY seq ASC",
            )
            .map_err(|e| VerifyError::Backend(e.to_string()))?;

        let events: Vec<(u64, u64, String, String, Option<String>)> = stmt
            .query_map([session], |row| {
                Ok((
                    to_u64(row.get::<_, i64>(0)?),
                    to_u64(row.get::<_, i64>(1)?),
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(|e| VerifyError::Backend(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| VerifyError::Backend(e.to_string()))?;

        if events.is_empty() {
            return Err(VerifyError::NotFound(session.to_string()));
        }

        for (i, (seq, _ts, kind, _payload, prev_digest)) in events.iter().enumerate() {
            if i == 0 {
                if prev_digest.is_some() {
                    return Ok(ChainVerification {
                        session: session.to_string(),
                        status: ChainStatus::Broken {
                            seq: *seq,
                            kind: kind.clone(),
                            reason: "first event should have NULL prev_digest".to_string(),
                        },
                    });
                }
                continue;
            }

            let (prev_seq, prev_ts, prev_kind, prev_payload, _) = &events[i - 1];
            let computed_digest =
                Self::compute_event_digest(*prev_seq, *prev_ts, prev_kind, prev_payload);

            if let Some(stored_digest) = prev_digest {
                if stored_digest != &computed_digest {
                    return Ok(ChainVerification {
                        session: session.to_string(),
                        status: ChainStatus::Broken {
                            seq: *seq,
                            kind: kind.clone(),
                            reason: format!(
                                "prev_digest mismatch: expected {computed_digest}, got {stored_digest}"
                            ),
                        },
                    });
                }
            } else {
                return Ok(ChainVerification {
                    session: session.to_string(),
                    status: ChainStatus::Broken {
                        seq: *seq,
                        kind: kind.clone(),
                        reason: "non-first event missing prev_digest".to_string(),
                    },
                });
            }
        }

        Ok(ChainVerification {
            session: session.to_string(),
            status: ChainStatus::Intact,
        })
    }
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}
