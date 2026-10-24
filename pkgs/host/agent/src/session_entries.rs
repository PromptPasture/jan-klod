//! Joins `host-storage` to the `SQLite` session store.

use std::sync::{Arc, Mutex};

use jk_session::store::{Entry, Store, StoreError};
use jk_wasm::guest_storage::{Entries, Row, StorageFault};

pub struct SessionEntries(Arc<Mutex<Store>>);

impl SessionEntries {
    #[must_use]
    pub const fn new(store: Arc<Mutex<Store>>) -> Self {
        Self(store)
    }

    /// A poisoned lock maps to `Backend`: refusing beats answering with partial data.
    fn with<T>(
        &self,
        call: impl FnOnce(&Store) -> Result<T, StoreError>,
    ) -> Result<T, StorageFault> {
        let store = self.0.lock().map_err(|_| StorageFault::Backend)?;
        call(&store).map_err(|err| fault(&err))
    }
}

/// The detail is logged because the contract carries none.
fn fault(err: &StoreError) -> StorageFault {
    match err {
        StoreError::NotFound => StorageFault::NotFound,
        StoreError::Backend { detail } => {
            eprintln!("WARN [core] host-storage backend error: {detail}");
            StorageFault::Backend
        }
    }
}

/// The `id` is dropped: the capability rebuilds it from the guest's request.
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
