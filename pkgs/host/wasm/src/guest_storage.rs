//! `host-storage` logic shared by every world that imports it. `bindgen!` generates a distinct
//!
//! `host-storage` trait and types per world, so the namespace scoping lives here over plain types
//! and each host writes a thin converting `impl`. Durable storage is reached via the [`Entries`] seam
//! (#180), declared here so neither this crate nor `jk-session` names the other.

use std::collections::HashMap;
use std::sync::Arc;

/// One row as a backing store holds it: the scoped namespace, no presentation applied.
///
/// Deliberately not the session store's own row type, to avoid the dependency the seam avoids.
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
/// The store has far more (event log, transcripts); a guest must not reach those through this.
/// `&self` because one store is shared by every guest and locks internally; `Send + Sync` because
/// a `wasmtime::Store`'s data must be.
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

/// An entry as a guest sees it.
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
/// Durability is opt-in per instance and the default is load-bearing: permission gates' standing
/// grants die with the process, and durable-by-default would turn "always allow" into "allow forever".
/// Namespaces are prefixed with the component id so a guest cannot name `session-abc` or a peer's
/// namespace; the core's own namespaces contain no `/`, so nothing a guest asks for collides.
pub struct GuestStorage {
    /// Where the entries live.
    backing: Backing,
    /// Whether namespaces are additionally keyed by session (#215); opt-in so "always allow" stays run-scoped.
    by_session: bool,
    /// The session a call belongs to, bound by the conductor at the top of a
    /// turn. `None` until something binds it.
    session: Option<String>,
}

impl GuestStorage {
    /// Not session-scoped: one namespace per component, for the whole run.
    #[must_use]
    pub const fn shared(backing: Backing) -> Self {
        Self {
            backing,
            by_session: false,
            session: None,
        }
    }

    /// Session-scoped: a namespace per component *and* session.
    #[must_use]
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
    /// A private map, for when no store is open (tests, offline harnesses).
    Ephemeral {
        /// (namespace, key) -> (value, created, updated).
        entries: HashMap<(String, String), (String, u64, u64)>,
        /// Monotonic clock, so timestamps order without a real clock.
        clock: u64,
    },
    /// A durable store, namespaced to the owning component.
    /// A durable store scoped to the owning component, reached through [`Entries`] (#180).
    Durable {
        /// Shared with the agent session that opened it, and with every
        /// other guest in the fleet.
        rows: Arc<dyn Entries>,
        /// The component id every namespace is prefixed with.
        owner: String,
    },
}

impl GuestStorage {
    /// The namespace a guest request actually reaches.
    ///
    /// # Errors
    /// [`StorageFault::Backend`] when session-scoped and no session is bound. Fails closed: falling back
    /// to the unscoped namespace would silently share state across sessions.
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

    /// Undo [`Self::scope`] so a returned entry names the namespace the guest asked for.
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
    ///
    /// # Errors
    /// [`StorageFault::Backend`] when the storage is session-scoped and no
    /// session is bound, or when the backing store refuses.
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
    ///
    /// # Errors
    /// [`StorageFault::Backend`] when the storage is session-scoped and no
    /// session is bound, or when the backing store refuses; [`StorageFault::NotFound`]
    /// when there is no such entry.
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
    ///
    /// # Errors
    /// [`StorageFault::Backend`] when the storage is session-scoped and no
    /// session is bound, or when the backing store refuses; [`StorageFault::NotFound`]
    /// when there is no such entry.
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
    ///
    /// # Errors
    /// [`StorageFault::Backend`] when the storage is session-scoped and no
    /// session is bound, or when the backing store refuses.
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
    ///
    /// # Errors
    /// [`StorageFault::Backend`] when the storage is session-scoped and no
    /// session is bound, or when the backing store refuses.
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

    /// One tool, two sessions, two plans.
    #[test]
    fn two_sessions_do_not_share_a_session_scoped_namespace() {
        let mut first = GuestStorage::per_session(ephemeral());
        first.bind_session("s1");
        first
            .set("plan".into(), "k".into(), "from s1".into())
            .expect("writes");

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

    /// Unscoped storage is unchanged (permission gates' standing grants rely on it).
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

    /// Fails closed: an unscoped fallback would share state across sessions unreported.
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

    /// A returned entry names the namespace the guest asked for, session prefix included.
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
