//! `host-storage`, once, for every world that imports it.
//!
//! This was private to [`crate::interceptor_host`] until a tool needed it too
//! (#215). It is lifted rather than copied for a reason specific to
//! `bindgen!`: each world generates its **own** `host-storage` trait and its
//! own `Entry` and `store-error` types, identical in shape and distinct to
//! the compiler. So the logic cannot live under the trait if two worlds are
//! to share it — it lives here over plain types, and each host writes a thin
//! `impl` that converts.
//!
//! What that leaves in each host is mechanical: call the method, map the
//! result. What lives here is the part worth having in one place — the
//! namespace scoping that keeps one guest out of another's data.
//!
//! # The seam under it
//!
//! Durable storage is reached through [`Entries`], a trait, rather than
//! through the session store directly (#180). The capability lives with the
//! component host; the SQLite store lives in `jk-session`, below it; and
//! neither should name the other. So this side declares what it needs — four
//! calls over a [`Row`] — and the crate that owns a session supplies
//! something that does them.
//!
//! The trait is written here, with the capability, rather than beside the
//! store: a seam takes the shape of whichever side declares it, and the side
//! that knows what is *needed* is this one. Written the other way round it
//! would be the store's whole API with a `dyn` in front of it.

use std::collections::HashMap;
use std::sync::Arc;

/// One row as a backing store holds it: the scoped namespace it was written
/// under, and no presentation applied.
///
/// Deliberately the same shape as a session store's own row, and deliberately
/// not that type. Naming it would be the dependency this seam exists to
/// avoid, and the duplication is five fields that have not changed since the
/// store was written.
pub struct Row {
    /// The namespace as stored — scoped, not the one the guest asked for.
    pub namespace: String,
    /// Key within the namespace.
    pub key: String,
    /// The opaque value.
    pub value: String,
    /// Creation time, in whatever clock the backing store keeps.
    pub created_at: u64,
    /// Last write time.
    pub updated_at: u64,
}

/// What `host-storage` needs from a durable store, and nothing more.
///
/// Four calls. The store behind them has many more — an event log, session
/// listing, fork points — and none of that is this capability's business: a
/// guest that can reach `host-storage` must not thereby be able to read a
/// transcript.
///
/// `&self` throughout, because the implementations share one store between
/// every guest in a fleet and do their own locking. An `&mut self` here would
/// make that sharing impossible to express.
///
/// `Send + Sync` because a `wasmtime::Store`'s data must be, and this ends up
/// inside one. Not a new constraint — the handle this replaces was an
/// `Arc<Mutex<_>>`, which is both — but it has to be said out loud now that
/// the type is erased.
pub trait Entries: Send + Sync {
    /// Upsert `value` and return the row as stored.
    ///
    /// # Errors
    /// [`StorageFault::Backend`] when the store refuses or cannot be reached.
    fn set(&self, namespace: &str, key: &str, value: &str) -> Result<Row, StorageFault>;

    /// Fetch one row.
    ///
    /// # Errors
    /// [`StorageFault::NotFound`] when there is no such row, or
    /// [`StorageFault::Backend`].
    fn get(&self, namespace: &str, key: &str) -> Result<Row, StorageFault>;

    /// Remove one row.
    ///
    /// # Errors
    /// [`StorageFault::NotFound`] when there is no such row, or
    /// [`StorageFault::Backend`].
    fn delete(&self, namespace: &str, key: &str) -> Result<(), StorageFault>;

    /// The most recently written rows in a namespace, newest first.
    ///
    /// # Errors
    /// [`StorageFault::Backend`] when the store refuses or cannot be reached.
    fn recent(&self, namespace: &str, limit: u32) -> Result<Vec<Row>, StorageFault>;
}

/// An entry as a guest sees it, before a world's own `Entry` is built from it.
pub struct StoredEntry {
    /// `<namespace>/<key>`, in the guest's own namespace.
    pub id: String,
    /// The namespace the guest asked for, not the one the host stored under.
    pub namespace: String,
    /// Key within the namespace.
    pub key: String,
    /// The opaque value.
    pub value: String,
    /// Creation time, in whatever clock the backing store keeps.
    pub created_at: u64,
    /// Last write time.
    pub updated_at: u64,
}

/// Why a storage call failed, in the two shapes the contract has.
#[derive(Debug)]
pub enum StorageFault {
    /// No entry at that namespace and key.
    NotFound,
    /// The backing store refused or could not be reached.
    Backend,
}

/// Where a guest's `host-storage` calls actually land.
///
/// Durability is opt-in per instance and the default is load-bearing:
/// permission gates' standing grants are documented as dying with the
/// process, and making every guest durable would quietly turn "always allow"
/// into "allow forever".
///
/// Every namespace is prefixed with the owning component's id, so sharing one
/// database does not let a guest name `session-abc` and read the transcript,
/// or name a peer's namespace and read its decisions. The core's own
/// namespaces contain no `/`, so nothing a guest can ask for collides with
/// them.
pub struct GuestStorage {
    /// Where the entries live.
    backing: Backing,
    /// Whether namespaces are additionally keyed by session (#215).
    ///
    /// Opt-in, and the default is not a detail: `interceptor-permission`'s
    /// standing grants are deliberately run-scoped, so turning this on for
    /// everything would silently narrow "always allow" to "always allow, in
    /// this session".
    by_session: bool,
    /// The session a call belongs to, bound by the conductor at the top of a
    /// turn. `None` until something binds it.
    session: Option<String>,
}

impl GuestStorage {
    /// Not session-scoped: one namespace per component, for the whole run.
    pub const fn shared(backing: Backing) -> Self {
        Self {
            backing,
            by_session: false,
            session: None,
        }
    }

    /// Session-scoped: a namespace per component *and* session.
    pub const fn per_session(backing: Backing) -> Self {
        Self {
            backing,
            by_session: true,
            session: None,
        }
    }

    /// Tell this storage which session subsequent calls belong to.
    pub fn bind_session(&mut self, session: &str) {
        self.session = Some(session.to_string());
    }
}

/// Where a guest's entries actually live.
pub enum Backing {
    /// A private map, for when no store is open (unit tests, offline
    /// harnesses).
    Ephemeral {
        /// (namespace, key) -> (value, created, updated).
        entries: HashMap<(String, String), (String, u64, u64)>,
        /// Monotonic clock, so timestamps order without a real clock.
        clock: u64,
    },
    /// A durable store, namespaced to the owning component.
    ///
    /// Reached through [`Entries`] rather than held directly, so this crate
    /// never names the crate that knows SQLite (#180).
    Durable {
        /// Shared with the [`AgentSession`](crate::AgentSession) that opened
        /// it, and with every other guest in the fleet.
        rows: Arc<dyn Entries>,
        /// The component id every namespace is prefixed with.
        owner: String,
    },
}

impl GuestStorage {
    /// The namespace a guest request actually reaches.
    ///
    /// # Errors
    /// [`StorageFault::Backend`] when this storage is session-scoped and
    /// nothing has bound a session. **Fail closed rather than fall back to
    /// the unscoped namespace**: the fallback would silently share one
    /// guest's state across every session, which is the exact property
    /// session scoping exists to prevent, and nothing would report it.
    fn scope(&self, namespace: &str) -> Result<String, StorageFault> {
        let within = match (&self.by_session, &self.session) {
            (false, _) => namespace.to_string(),
            (true, Some(session)) => format!("s/{session}/{namespace}"),
            (true, None) => {
                eprintln!(
                    "WARN [core] host-storage is session-scoped and no session is bound; \
                     refusing rather than sharing state across sessions"
                );
                return Err(StorageFault::Backend);
            }
        };
        Ok(match &self.backing {
            Backing::Ephemeral { .. } => within,
            Backing::Durable { owner, .. } => format!("ext/{owner}/{within}"),
        })
    }

    /// Undo [`Self::scope`], so a returned entry names the namespace the
    /// guest asked for rather than what the host stored under.
    fn unscope(&self, namespace: &str) -> String {
        let outer = match &self.backing {
            Backing::Ephemeral { .. } => namespace,
            Backing::Durable { owner, .. } => namespace
                .strip_prefix(&format!("ext/{owner}/"))
                .unwrap_or(namespace),
        };
        match (&self.by_session, &self.session) {
            (true, Some(session)) => outer
                .strip_prefix(&format!("s/{session}/"))
                .unwrap_or(outer)
                .to_string(),
            _ => outer.to_string(),
        }
    }

    /// Present a stored row under the namespace the guest asked for.
    fn present(&self, row: Row) -> StoredEntry {
        let namespace = self.unscope(&row.namespace);
        StoredEntry {
            id: format!("{namespace}/{}", row.key),
            namespace,
            key: row.key,
            value: row.value,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }

    /// Upsert `value`, returning the entry as stored.
    pub fn set(
        &mut self,
        namespace: String,
        key: String,
        value: String,
    ) -> Result<StoredEntry, StorageFault> {
        let scoped = self.scope(&namespace)?;
        match &mut self.backing {
            Backing::Ephemeral { entries, clock } => {
                *clock += 1;
                let now = *clock;
                let created = entries
                    .get(&(scoped.clone(), key.clone()))
                    .map_or(now, |(_, created, _)| *created);
                entries.insert((scoped, key.clone()), (value.clone(), created, now));
                Ok(StoredEntry {
                    id: format!("{namespace}/{key}"),
                    namespace,
                    key,
                    value,
                    created_at: created,
                    updated_at: now,
                })
            }
            Backing::Durable { rows, .. } => {
                let row = rows.set(&scoped, &key, &value)?;
                Ok(self.present(row))
            }
        }
    }

    /// Fetch one entry.
    pub fn get(&self, namespace: &str, key: &str) -> Result<StoredEntry, StorageFault> {
        let scoped = self.scope(namespace)?;
        match &self.backing {
            Backing::Ephemeral { entries, .. } => entries
                .get(&(scoped, key.to_string()))
                .map(|(value, created, updated)| StoredEntry {
                    id: format!("{namespace}/{key}"),
                    namespace: namespace.to_string(),
                    key: key.to_string(),
                    value: value.clone(),
                    created_at: *created,
                    updated_at: *updated,
                })
                .ok_or(StorageFault::NotFound),
            Backing::Durable { rows, .. } => {
                let row = rows.get(&scoped, key)?;
                Ok(self.present(row))
            }
        }
    }

    /// Remove one entry.
    pub fn delete(&mut self, namespace: &str, key: &str) -> Result<(), StorageFault> {
        let scoped = self.scope(namespace)?;
        match &mut self.backing {
            Backing::Ephemeral { entries, .. } => entries
                .remove(&(scoped, key.to_string()))
                .map(|_| ())
                .ok_or(StorageFault::NotFound),
            Backing::Durable { rows, .. } => rows.delete(&scoped, key),
        }
    }

    /// Every entry in a namespace, unordered.
    pub fn entries_in(&self, namespace: &str) -> Result<Vec<StoredEntry>, StorageFault> {
        let scoped = self.scope(namespace)?;
        match &self.backing {
            Backing::Ephemeral { entries, .. } => Ok(entries
                .iter()
                .filter(|((ns, _), _)| *ns == scoped)
                .map(|((_, key), (value, created, updated))| StoredEntry {
                    id: format!("{namespace}/{key}"),
                    namespace: namespace.to_string(),
                    key: key.clone(),
                    value: value.clone(),
                    created_at: *created,
                    updated_at: *updated,
                })
                .collect()),
            Backing::Durable { rows, .. } => Ok(rows
                .recent(&scoped, u32::MAX)?
                .into_iter()
                .map(|row| self.present(row))
                .collect()),
        }
    }

    /// The most recently written entries in a namespace, newest first.
    pub fn recent(&self, namespace: &str, limit: u32) -> Result<Vec<StoredEntry>, StorageFault> {
        let mut entries = self.entries_in(namespace)?;
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        entries.truncate(limit as usize);
        Ok(entries)
    }
}

#[cfg(test)]
mod tests {
    use super::{Backing, GuestStorage, StorageFault};
    use std::collections::HashMap;

    fn ephemeral() -> Backing {
        Backing::Ephemeral {
            entries: HashMap::new(),
            clock: 0,
        }
    }

    /// The property the whole option exists for: one tool, two sessions, two
    /// plans.
    #[test]
    fn two_sessions_do_not_share_a_session_scoped_namespace() {
        let mut first = GuestStorage::per_session(ephemeral());
        first.bind_session("s1");
        first
            .set("plan".into(), "k".into(), "from s1".into())
            .expect("writes");

        // Same storage, rebound — a fleet outlives a session, so this is the
        // real sequence rather than two objects.
        first.bind_session("s2");
        assert!(
            matches!(first.get("plan", "k"), Err(StorageFault::NotFound)),
            "s2 read s1's entry"
        );

        first
            .set("plan".into(), "k".into(), "from s2".into())
            .expect("writes");
        first.bind_session("s1");
        assert_eq!(
            first.get("plan", "k").expect("s1 still has its own").value,
            "from s1",
            "s2's write overwrote s1's"
        );
    }

    /// Unscoped storage is unchanged by any of this — the default a
    /// permission gate's standing grants rely on.
    #[test]
    fn shared_storage_ignores_the_session_entirely() {
        let mut shared = GuestStorage::shared(ephemeral());
        shared.bind_session("s1");
        shared
            .set("grants".into(), "k".into(), "always".into())
            .expect("writes");
        shared.bind_session("s2");
        assert_eq!(
            shared.get("grants", "k").expect("still there").value,
            "always",
            "a run-scoped grant stopped crossing sessions, which would narrow \
             `always allow` to one session"
        );
    }

    /// Fail closed. The tempting alternative — fall back to the unscoped
    /// namespace — shares one guest's state across every session and reports
    /// nothing, which is the exact failure session scoping prevents.
    #[test]
    fn session_scoped_storage_with_no_session_refuses() {
        let mut unbound = GuestStorage::per_session(ephemeral());
        assert!(matches!(
            unbound.set("plan".into(), "k".into(), "v".into()),
            Err(StorageFault::Backend)
        ));
        assert!(matches!(
            unbound.get("plan", "k"),
            Err(StorageFault::Backend)
        ));
    }

    /// A returned entry names the namespace the guest asked for, not the one
    /// the host stored under — session prefix included.
    #[test]
    fn a_session_scoped_entry_is_presented_unscoped() {
        let mut storage = GuestStorage::per_session(ephemeral());
        storage.bind_session("s1");
        let entry = storage
            .set("plan".into(), "k".into(), "v".into())
            .expect("writes");
        assert_eq!(entry.namespace, "plan");
        assert_eq!(entry.id, "plan/k");
    }
}
