//! Where the two halves of storage are joined (#180).
//!
//! [`guest_storage`](crate::guest_storage) declares what `host-storage`
//! needs; `jk-session` provides a SQLite store. Neither names the other, so
//! something has to say they fit — and that something is this crate, which
//! owns an [`AgentSession`](crate::AgentSession) and is therefore the only
//! one that already knows both.
//!
//! It is a newtype rather than an `impl` on the store, and that is the part
//! worth reading twice. Once #181 moves the capability into `jk-wasm` and
//! #182 moves this into `jk-agent`, `jk-agent` will own neither the trait nor
//! `Store` — so an `impl Entries for Arc<Mutex<Store>>` would stop compiling
//! the moment the split it was written for happens. The wrapper is what makes
//! the arrangement survive its own next slice.

use std::sync::{Arc, Mutex};

use crate::guest_storage::{Entries, Row, StorageFault};
use crate::store::{Entry, Store, StoreError};

/// The session store, offered to guests as the four calls `host-storage` has.
///
/// Holds the same handle the session does, so a guest's writes and the log's
/// are one database and one lock.
pub struct SessionEntries(Arc<Mutex<Store>>);

impl SessionEntries {
    /// Offer `store` to `host-storage`.
    #[must_use]
    pub const fn new(store: Arc<Mutex<Store>>) -> Self {
        Self(store)
    }

    /// Run `call` against the store, mapping both failure kinds.
    ///
    /// A poisoned lock is a [`StorageFault::Backend`]: some other thread
    /// panicked holding the store, and answering a guest with stale or
    /// partial data would be worse than refusing.
    fn with<T>(
        &self,
        call: impl FnOnce(&Store) -> Result<T, StoreError>,
    ) -> Result<T, StorageFault> {
        let store = self.0.lock().map_err(|_| StorageFault::Backend)?;
        call(&store).map_err(|err| fault(&err))
    }
}

/// A store error as the capability's two-shape fault, with the detail logged
/// rather than dropped — the contract's error carries none.
fn fault(err: &StoreError) -> StorageFault {
    match err {
        StoreError::NotFound => StorageFault::NotFound,
        StoreError::Backend { detail } => {
            eprintln!("WARN [core] host-storage backend error: {detail}");
            StorageFault::Backend
        }
    }
}

/// A stored entry as the seam's plain row.
///
/// The `id` is dropped on purpose: it is `<namespace>/<key>` of the *scoped*
/// namespace, and the capability rebuilds it from the namespace the guest
/// asked for. Carrying it across would be carrying the answer to a question
/// the other side is about to ask differently.
fn row(entry: Entry) -> Row {
    Row {
        namespace: entry.namespace,
        key: entry.key,
        value: entry.value,
        created_at: entry.created_at,
        updated_at: entry.updated_at,
    }
}

impl Entries for SessionEntries {
    fn set(&self, namespace: &str, key: &str, value: &str) -> Result<Row, StorageFault> {
        self.with(|store| store.set(namespace, key, value)).map(row)
    }

    fn get(&self, namespace: &str, key: &str) -> Result<Row, StorageFault> {
        self.with(|store| store.get(namespace, key)).map(row)
    }

    fn delete(&self, namespace: &str, key: &str) -> Result<(), StorageFault> {
        self.with(|store| store.delete(namespace, key))
    }

    fn recent(&self, namespace: &str, limit: u32) -> Result<Vec<Row>, StorageFault> {
        self.with(|store| store.recent(namespace, limit))
            .map(|rows| rows.into_iter().map(row).collect())
    }
}
