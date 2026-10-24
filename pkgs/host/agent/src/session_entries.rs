//! Joins `host-storage` to the SQLite session store (#180). A newtype rather than an `impl` on `Store`, because the
//! planned split (#181, #182) would leave this crate owning neither the trait nor `Store`.

use std::sync::{Arc, Mutex};

use jk_session::store::{Entry, Store, StoreError};
use jk_wasm::guest_storage::{Entries, Row, StorageFault};

/// The session store, offered to guests as the four calls `host-storage` has.
/// Shares the session's handle, so guest writes and the log use one lock.
pub struct SessionEntries(Arc<Mutex<Store>>);

impl SessionEntries {
    /// Offer `store` to `host-storage`.
    #[must_use]
    pub const fn new(store: Arc<Mutex<Store>>) -> Self {
        Self(store)
    }

    /// Run `call` against the store, mapping both failure kinds.
    /// A poisoned lock maps to `Backend`: refusing beats answering with partial data.
    fn with<T>(
        &self,
        call: impl FnOnce(&Store) -> Result<T, StoreError>,
    ) -> Result<T, StorageFault> {
        let store = self.0.lock().map_err(|_| StorageFault::Backend)?;
        call(&store).map_err(|err| fault(&err))
    }
}

/// Maps a store error to the capability's fault. The detail is logged because the contract carries none.
fn fault(err: &StoreError) -> StorageFault {
    match err {
        StoreError::NotFound => StorageFault::NotFound,
        StoreError::Backend { detail } => {
            eprintln!("WARN [core] host-storage backend error: {detail}");
            StorageFault::Backend
        }
    }
}

/// A stored entry as the seam's plain row. The `id` is dropped: it names the
/// scoped namespace, which the capability rebuilds from the guest's request.
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
