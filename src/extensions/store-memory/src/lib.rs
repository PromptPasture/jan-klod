//! `store-memory` — the first real Rust guest: an in-memory implementation of
//! the `memory-store` interface, plus the universal `extension-lifecycle`.
//!
//! It is the development-default store (`enabled: true` out of the box): zero
//! setup, no persistence — everything lives in a process-local map and is gone
//! when the component is dropped. It exercises the full guest round-trip the
//! core wired up: it imports `host-log` (structured logging back to the core)
//! and `host-config` (reads its own `config.yaml` section), and exports
//! lifecycle + storage over the Component Model.

// Generated Component-Model bindings; lint exemptions (incl. the `unsafe` ABI
// shims) scoped to the macro output.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod bindings {
    wit_bindgen::generate!({
        world: "store-world",
        path: "../../../wit",
    });
}

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use bindings::exports::jan_klod::interfaces::extension_lifecycle::{
    ExtensionContext, Guest as Lifecycle, HealthStatus,
};
use bindings::exports::jan_klod::interfaces::memory_store::{Entry, Guest as MemoryStore, StoreError};
use bindings::jan_klod::interfaces::host_config;
use bindings::jan_klod::interfaces::host_log::{self, LogLevel};

/// One stored value plus its metadata. The owning namespace and key are the map
/// keys, so they are not duplicated here.
struct StoredEntry {
    /// Opaque row id, assigned once on first insert.
    id: String,
    /// JSON-encoded payload.
    value: String,
    /// Unix seconds at first insert.
    created_at: u64,
    /// Unix seconds at the most recent write.
    updated_at: u64,
}

thread_local! {
    /// namespace -> key -> entry. A component instance is single-threaded, so a
    /// thread-local `RefCell` is the idiomatic global-state holder.
    static STORE: RefCell<HashMap<String, HashMap<String, StoredEntry>>> =
        RefCell::new(HashMap::new());
    /// Monotonic counter backing opaque row ids.
    static SEQ: RefCell<u64> = const { RefCell::new(0) };
}

/// Current Unix time in seconds (`wasi:clocks` under the hood). Pre-epoch clocks
/// are impossible in practice, so a skewed clock degrades to `0` rather than
/// trapping.
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Allocate the next opaque row id.
fn next_id() -> String {
    SEQ.with(|s| {
        let mut n = s.borrow_mut();
        *n += 1;
        format!("row-{n}")
    })
}

/// Build a wire `Entry`. `value` is blanked for listings that omit the payload.
fn to_entry(namespace: &str, key: &str, e: &StoredEntry, include_value: bool) -> Entry {
    Entry {
        id: e.id.clone(),
        namespace: namespace.to_owned(),
        key: key.to_owned(),
        value: if include_value {
            e.value.clone()
        } else {
            String::new()
        },
        created_at: e.created_at,
        updated_at: e.updated_at,
    }
}

/// Forward a line to the core's log pipeline, tagged with this component's name.
fn log(level: LogLevel, message: &str) {
    host_log::log(level, "store-memory", message, &[]);
}

/// Saturating `u32` -> `usize` for `limit` arguments.
fn cap(limit: u32) -> usize {
    usize::try_from(limit).unwrap_or(usize::MAX)
}

/// The single type that implements every interface `store-world` exports.
struct Component;

impl Lifecycle for Component {
    fn init(ctx: ExtensionContext) -> Result<(), String> {
        // Read our own config section back through host-config — proves the
        // host capability round-trips from inside the guest.
        let config = host_config::all().unwrap_or_else(|_| "{}".to_owned());
        log(
            LogLevel::Info,
            &format!(
                "init id={} version={} config={config}",
                ctx.id, ctx.version
            ),
        );
        Ok(())
    }

    fn start() -> Result<(), String> {
        log(LogLevel::Info, "started; in-memory backend ready");
        Ok(())
    }

    fn stop() {
        log(LogLevel::Info, "stopping; clearing in-memory data");
        STORE.with(|s| s.borrow_mut().clear());
    }

    fn health() -> HealthStatus {
        HealthStatus::Up
    }
}

impl MemoryStore for Component {
    fn set(namespace: String, key: String, value: String) -> Result<Entry, StoreError> {
        let ts = now();
        let (id, created_at) = STORE.with(|s| {
            let mut store = s.borrow_mut();
            let ns = store.entry(namespace.clone()).or_default();
            if let Some(existing) = ns.get_mut(&key) {
                existing.value.clone_from(&value);
                existing.updated_at = ts;
                (existing.id.clone(), existing.created_at)
            } else {
                let id = next_id();
                ns.insert(
                    key.clone(),
                    StoredEntry {
                        id: id.clone(),
                        value: value.clone(),
                        created_at: ts,
                        updated_at: ts,
                    },
                );
                (id, ts)
            }
        });
        Ok(Entry {
            id,
            namespace,
            key,
            value,
            created_at,
            updated_at: ts,
        })
    }

    fn get(namespace: String, key: String) -> Result<Entry, StoreError> {
        STORE.with(|s| {
            s.borrow()
                .get(&namespace)
                .and_then(|ns| ns.get(&key))
                .map(|e| to_entry(&namespace, &key, e, true))
                .ok_or(StoreError::NotFound)
        })
    }

    fn delete(namespace: String, key: String) -> Result<(), StoreError> {
        STORE.with(|s| {
            if let Some(ns) = s.borrow_mut().get_mut(&namespace) {
                ns.remove(&key);
            }
        });
        Ok(())
    }

    fn list_keys(namespace: String) -> Result<Vec<Entry>, StoreError> {
        Ok(STORE.with(|s| {
            s.borrow().get(&namespace).map_or_else(Vec::new, |ns| {
                ns.iter()
                    .map(|(k, e)| to_entry(&namespace, k, e, false))
                    .collect()
            })
        }))
    }

    fn recent(namespace: String, limit: u32) -> Result<Vec<Entry>, StoreError> {
        Ok(STORE.with(|s| {
            let mut entries: Vec<Entry> = s.borrow().get(&namespace).map_or_else(Vec::new, |ns| {
                ns.iter()
                    .map(|(k, e)| to_entry(&namespace, k, e, true))
                    .collect()
            });
            entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
            entries.truncate(cap(limit));
            entries
        }))
    }

    fn search(namespace: String, query: String, limit: u32) -> Result<Vec<Entry>, StoreError> {
        let needle = query.to_lowercase();
        Ok(STORE.with(|s| {
            let mut entries: Vec<Entry> = s.borrow().get(&namespace).map_or_else(Vec::new, |ns| {
                ns.iter()
                    .filter(|(k, e)| {
                        k.to_lowercase().contains(&needle)
                            || e.value.to_lowercase().contains(&needle)
                    })
                    .map(|(k, e)| to_entry(&namespace, k, e, true))
                    .collect()
            });
            entries.sort_by_key(|e| std::cmp::Reverse(e.updated_at));
            entries.truncate(cap(limit));
            entries
        }))
    }

    fn purge_namespace(namespace: String) -> Result<(), StoreError> {
        STORE.with(|s| s.borrow_mut().remove(&namespace));
        Ok(())
    }
}

// The `export!` macro emits the component's `unsafe extern "C"` ABI shims at its
// call site, so scope the binding lints (incl. `unsafe_code`) to this glue too.
#[allow(unsafe_code, missing_docs, clippy::all, clippy::pedantic, clippy::nursery)]
mod glue {
    use crate::{bindings, Component};
    bindings::export!(Component with_types_in bindings);
}
