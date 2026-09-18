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

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::store::Store as PersistentStore;

/// An entry as a guest sees it, before a world's own `Entry` is built from it.
pub(crate) struct StoredEntry {
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
pub(crate) enum StorageFault {
    /// No entry at that namespace and key.
    NotFound,
    /// The backing store refused or could not be reached.
    Backend,
}

impl StorageFault {
    /// A store error, with its detail logged rather than dropped — the
    /// contract's error carries none.
    fn from_store(err: &crate::store::StoreError) -> Self {
        match err {
            crate::store::StoreError::NotFound => Self::NotFound,
            crate::store::StoreError::Backend { detail } => {
                eprintln!("WARN [core] host-storage backend error: {detail}");
                Self::Backend
            }
        }
    }
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
pub(crate) enum GuestStorage {
    /// A private map, for when no store is open (unit tests, offline
    /// harnesses).
    Ephemeral {
        /// (namespace, key) -> (value, created, updated).
        entries: HashMap<(String, String), (String, u64, u64)>,
        /// Monotonic clock, so timestamps order without a real clock.
        clock: u64,
    },
    /// The core's store, namespaced to the owning component.
    Durable {
        /// Shared with the [`AgentSession`](crate::AgentSession) that opened
        /// it.
        store: Arc<Mutex<PersistentStore>>,
        /// The component id every namespace is prefixed with.
        owner: String,
    },
}

impl GuestStorage {
    /// The namespace a guest request actually reaches.
    fn scope(&self, namespace: &str) -> String {
        match self {
            Self::Ephemeral { .. } => namespace.to_string(),
            Self::Durable { owner, .. } => format!("ext/{owner}/{namespace}"),
        }
    }

    /// Undo [`Self::scope`], so a returned entry names the namespace the
    /// guest asked for rather than what the host stored under.
    fn unscope(&self, namespace: &str) -> String {
        match self {
            Self::Ephemeral { .. } => namespace.to_string(),
            Self::Durable { owner, .. } => namespace
                .strip_prefix(&format!("ext/{owner}/"))
                .unwrap_or(namespace)
                .to_string(),
        }
    }

    /// Present a stored row under the namespace the guest asked for.
    fn present(&self, entry: crate::store::Entry) -> StoredEntry {
        let namespace = self.unscope(&entry.namespace);
        StoredEntry {
            id: format!("{namespace}/{}", entry.key),
            namespace,
            key: entry.key,
            value: entry.value,
            created_at: entry.created_at,
            updated_at: entry.updated_at,
        }
    }

    /// Upsert `value`, returning the entry as stored.
    pub(crate) fn set(
        &mut self,
        namespace: String,
        key: String,
        value: String,
    ) -> Result<StoredEntry, StorageFault> {
        let scoped = self.scope(&namespace);
        match self {
            Self::Ephemeral { entries, clock } => {
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
            Self::Durable { store, .. } => {
                let entry = store
                    .lock()
                    .map_err(|_| StorageFault::Backend)?
                    .set(&scoped, &key, &value)
                    .map_err(|err| StorageFault::from_store(&err))?;
                Ok(self.present(entry))
            }
        }
    }

    /// Fetch one entry.
    pub(crate) fn get(&self, namespace: &str, key: &str) -> Result<StoredEntry, StorageFault> {
        let scoped = self.scope(namespace);
        match self {
            Self::Ephemeral { entries, .. } => entries
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
            Self::Durable { store, .. } => {
                let entry = store
                    .lock()
                    .map_err(|_| StorageFault::Backend)?
                    .get(&scoped, key)
                    .map_err(|err| StorageFault::from_store(&err))?;
                Ok(self.present(entry))
            }
        }
    }

    /// Remove one entry.
    pub(crate) fn delete(&mut self, namespace: &str, key: &str) -> Result<(), StorageFault> {
        let scoped = self.scope(namespace);
        match self {
            Self::Ephemeral { entries, .. } => entries
                .remove(&(scoped, key.to_string()))
                .map(|_| ())
                .ok_or(StorageFault::NotFound),
            Self::Durable { store, .. } => store
                .lock()
                .map_err(|_| StorageFault::Backend)?
                .delete(&scoped, key)
                .map_err(|err| StorageFault::from_store(&err)),
        }
    }

    /// Every entry in a namespace, unordered.
    pub(crate) fn entries_in(&self, namespace: &str) -> Result<Vec<StoredEntry>, StorageFault> {
        let scoped = self.scope(namespace);
        match self {
            Self::Ephemeral { entries, .. } => Ok(entries
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
            Self::Durable { store, .. } => {
                let rows = store
                    .lock()
                    .map_err(|_| StorageFault::Backend)?
                    .recent(&scoped, u32::MAX)
                    .map_err(|err| StorageFault::from_store(&err))?;
                Ok(rows.into_iter().map(|row| self.present(row)).collect())
            }
        }
    }

    /// The most recently written entries in a namespace, newest first.
    pub(crate) fn recent(
        &self,
        namespace: &str,
        limit: u32,
    ) -> Result<Vec<StoredEntry>, StorageFault> {
        let mut entries = self.entries_in(namespace)?;
        entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
        entries.truncate(limit as usize);
        Ok(entries)
    }
}
