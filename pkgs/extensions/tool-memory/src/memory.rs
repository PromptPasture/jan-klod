//! The memory itself: storing, recalling, and forgetting facts.
//!
//! Pure and host-testable. Each fact has a unique ID, timestamp, key, and value.
//! Facts are stored via the host-storage interface in the "memory" namespace.
//! This module handles the JSON serialization, parsing, and operations.

use chrono::Utc;
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// A stored fact with metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fact {
    /// Unique identifier for the fact.
    pub id: u64,
    /// The key to store/recall the fact by.
    pub key: String,
    /// The fact value.
    pub value: String,
    /// ISO 8601 timestamp of creation.
    pub created_at: String,
    /// ISO 8601 timestamp of last update.
    pub updated_at: String,
}

impl Fact {
    fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "key": self.key,
            "value": self.value,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
        })
    }

    fn from_json(obj: &Value) -> Option<Self> {
        let id = obj.get("id")?.as_u64()?;
        let key = obj.get("key")?.as_str()?.to_string();
        let value = obj.get("value")?.as_str()?.to_string();
        let created_at = obj.get("created_at")?.as_str()?.to_string();
        let updated_at = obj.get("updated_at")?.as_str()?.to_string();
        Some(Self {
            id,
            key,
            value,
            created_at,
            updated_at,
        })
    }
}

/// Why an operation was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    /// The arguments were not a recognised operation.
    BadArguments(String),
    /// A key was required but not provided.
    MissingKey,
    /// The key was not found during forget or recall-specific.
    NotFound(String),
    /// Invalid JSON in the operation arguments.
    InvalidJson,
}

impl std::fmt::Display for MemoryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::BadArguments(detail) => write!(f, "not a memory operation: {detail}"),
            Self::MissingKey => write!(f, "operation requires a key"),
            Self::NotFound(key) => write!(f, "key '{key}' not found"),
            Self::InvalidJson => write!(f, "invalid JSON in arguments"),
        }
    }
}

/// The in-memory facts store, keyed by their key.
#[derive(Debug, Clone, Default)]
pub struct Memory {
    /// All facts, keyed by their original key. Multiple facts can have the same key
    /// if updated; we keep the most recent for each key.
    facts: BTreeMap<String, Fact>,
    /// The next ID to assign.
    next_id: u64,
}

impl Memory {
    /// Parse memory from stored JSON representation (or create empty if None).
    #[must_use]
    pub fn parse(stored: Option<&str>) -> Self {
        stored.map_or_else(Self::default, |json_str| {
            match serde_json::from_str::<Value>(json_str) {
                Ok(Value::Array(items)) => {
                    let mut facts = BTreeMap::new();
                    let mut max_id = 0u64;
                    for item in items {
                        if let Some(fact) = Fact::from_json(&item) {
                            max_id = max_id.max(fact.id);
                            facts.insert(fact.key.clone(), fact);
                        }
                    }
                    Self {
                        facts,
                        next_id: max_id + 1,
                    }
                }
                _ => Self::default(),
            }
        })
    }

    /// Render memory as JSON array.
    #[must_use]
    pub fn render(&self) -> String {
        let items: Vec<Value> = self.facts.values().map(Fact::to_json).collect();
        serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
    }

    /// Store a fact. Returns the stored fact.
    ///
    /// # Errors
    ///
    /// `MemoryError::BadArguments` when `key` is empty.
    pub fn store(&mut self, key: String, value: String) -> Result<Fact, MemoryError> {
        if key.is_empty() {
            return Err(MemoryError::BadArguments("key cannot be empty".to_string()));
        }

        let now = chrono_now();
        let (id, created_at) = if let Some(existing) = self.facts.get(&key) {
            (existing.id, existing.created_at.clone())
        } else {
            let id = self.next_id;
            self.next_id += 1;
            (id, now.clone())
        };

        let fact = Fact {
            id,
            key: key.clone(),
            value,
            created_at,
            updated_at: now,
        };

        self.facts.insert(key, fact.clone());
        Ok(fact)
    }

    /// Recall facts. If key is provided, return that fact. Otherwise, return
    /// the 10 most recent facts in reverse chronological order.
    ///
    /// # Errors
    ///
    /// `MemoryError::NotFound` when `key` names no stored fact.
    pub fn recall(&self, key: Option<String>) -> Result<Vec<Fact>, MemoryError> {
        key.map_or_else(
            || {
                // Return up to 10 most recent facts (by updated_at, descending)
                let mut all_facts: Vec<_> = self.facts.values().cloned().collect();
                all_facts.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
                Ok(all_facts.into_iter().take(10).collect())
            },
            |k| {
                self.facts.get(&k).map_or_else(
                    || Err(MemoryError::NotFound(k)),
                    |fact| Ok(vec![fact.clone()]),
                )
            },
        )
    }

    /// Forget a fact. Returns the forgotten fact's key.
    ///
    /// # Errors
    ///
    /// `MemoryError::NotFound` when `key` names no stored fact.
    pub fn forget(&mut self, key: String) -> Result<String, MemoryError> {
        if self.facts.remove(&key).is_some() {
            Ok(key)
        } else {
            Err(MemoryError::NotFound(key))
        }
    }

    /// Apply an operation from JSON arguments.
    ///
    /// # Errors
    ///
    /// `MemoryError::InvalidJson` when `arguments` is not JSON, `BadArguments`
    /// when a required field is missing or `op` is unknown, and whatever the
    /// operation itself returns (`NotFound` for `recall`/`forget` of an
    /// absent key).
    pub fn apply(&mut self, arguments: &str) -> Result<String, MemoryError> {
        let args: Value = serde_json::from_str(arguments).map_err(|_| MemoryError::InvalidJson)?;

        let op = args
            .get("op")
            .and_then(Value::as_str)
            .ok_or_else(|| MemoryError::BadArguments("missing 'op' field".to_string()))?;

        match op {
            "store" => {
                let key = args
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or(MemoryError::MissingKey)?
                    .to_string();
                let value = args
                    .get("value")
                    .and_then(Value::as_str)
                    .ok_or_else(|| MemoryError::BadArguments("missing 'value'".to_string()))?
                    .to_string();
                let fact = self.store(key, value)?;
                Ok(serde_json::to_string(&fact.to_json()).unwrap_or_default())
            }
            "recall" => {
                let key = args.get("key").and_then(Value::as_str).map(String::from);
                let facts = self.recall(key)?;
                let response: Vec<Value> = facts.iter().map(Fact::to_json).collect();
                Ok(serde_json::to_string(&response).unwrap_or_default())
            }
            "forget" => {
                let key = args
                    .get("key")
                    .and_then(Value::as_str)
                    .ok_or(MemoryError::MissingKey)?
                    .to_string();
                let removed_key = self.forget(key)?;
                Ok(serde_json::to_string(&json!({"key": removed_key})).unwrap_or_default())
            }
            _ => Err(MemoryError::BadArguments(format!(
                "unknown operation: {op}"
            ))),
        }
    }
}

/// Get current timestamp in ISO 8601 format.
fn chrono_now() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_store_and_recall() {
        let mut memory = Memory::default();

        // Store a fact
        let result = memory
            .apply(r#"{"op":"store","key":"foo","value":"bar"}"#)
            .expect("store should succeed");
        let stored: Value = serde_json::from_str(&result).expect("result is JSON");
        assert_eq!(stored.get("key").and_then(Value::as_str), Some("foo"));
        assert_eq!(stored.get("value").and_then(Value::as_str), Some("bar"));

        // Recall the fact by key
        let result = memory
            .apply(r#"{"op":"recall","key":"foo"}"#)
            .expect("recall should succeed");
        let recalled: Vec<Value> = serde_json::from_str(&result).expect("result is JSON");
        assert_eq!(recalled.len(), 1);
        assert_eq!(recalled[0].get("key").and_then(Value::as_str), Some("foo"));
        assert_eq!(
            recalled[0].get("value").and_then(Value::as_str),
            Some("bar")
        );
    }

    #[test]
    fn test_recall_without_key_returns_all() {
        let mut memory = Memory::default();

        memory
            .apply(r#"{"op":"store","key":"first","value":"1"}"#)
            .expect("store first");
        memory
            .apply(r#"{"op":"store","key":"second","value":"2"}"#)
            .expect("store second");
        memory
            .apply(r#"{"op":"store","key":"third","value":"3"}"#)
            .expect("store third");

        let result = memory
            .apply(r#"{"op":"recall"}"#)
            .expect("recall all should succeed");
        let recalled: Vec<Value> = serde_json::from_str(&result).expect("result is JSON");
        assert_eq!(recalled.len(), 3);
    }

    #[test]
    fn test_forget_removes_fact() {
        let mut memory = Memory::default();

        memory
            .apply(r#"{"op":"store","key":"target","value":"data"}"#)
            .expect("store should succeed");

        let result = memory
            .apply(r#"{"op":"forget","key":"target"}"#)
            .expect("forget should succeed");
        let forgotten: Value = serde_json::from_str(&result).expect("result is JSON");
        assert_eq!(forgotten.get("key").and_then(Value::as_str), Some("target"));

        // Verify the fact is gone
        let recall_result = memory.apply(r#"{"op":"recall","key":"target"}"#);
        assert!(recall_result.is_err());
    }

    #[test]
    fn test_forget_nonexistent_fails() {
        let mut memory = Memory::default();

        let result = memory.apply(r#"{"op":"forget","key":"nonexistent"}"#);
        assert!(result.is_err());
    }

    #[test]
    fn test_memory_persistence_across_instantiation() {
        // Simulate storing a fact and then re-instantiating the memory
        let mut memory1 = Memory::default();
        memory1
            .apply(r#"{"op":"store","key":"persist","value":"data"}"#)
            .expect("store should succeed");

        // Serialize to storage format
        let serialized = memory1.render();

        // Create new memory instance from serialized data
        let mut memory2 = Memory::parse(Some(&serialized));

        // Verify the fact is still there
        let result = memory2
            .apply(r#"{"op":"recall","key":"persist"}"#)
            .expect("recall should succeed");
        let recalled: Vec<Value> = serde_json::from_str(&result).expect("result is JSON");
        assert_eq!(recalled.len(), 1);
        assert_eq!(
            recalled[0].get("value").and_then(Value::as_str),
            Some("data")
        );
    }

    #[test]
    fn test_update_preserves_created_at() {
        let mut memory = Memory::default();

        // Store initial fact
        let result1 = memory
            .apply(r#"{"op":"store","key":"test_key","value":"initial"}"#)
            .expect("first store should succeed");
        let stored1: Value = serde_json::from_str(&result1).expect("result is JSON");
        let created_at1 = stored1.get("created_at").and_then(Value::as_str);

        // Update the same fact with a new value
        let result2 = memory
            .apply(r#"{"op":"store","key":"test_key","value":"updated"}"#)
            .expect("second store should succeed");
        let stored2: Value = serde_json::from_str(&result2).expect("result is JSON");
        let created_at2 = stored2.get("created_at").and_then(Value::as_str);
        let updated_at2 = stored2.get("updated_at").and_then(Value::as_str);

        // Verify created_at is preserved
        assert_eq!(
            created_at1, created_at2,
            "created_at should not change on update"
        );
        assert_eq!(
            stored2.get("value").and_then(Value::as_str),
            Some("updated"),
            "value should be updated"
        );
        // updated_at should be different (or at least it can be the same if executed in same millisecond,
        // but conceptually it should represent the time of the last update)
        assert!(
            updated_at2.is_some(),
            "updated_at should be present in updated fact"
        );
    }
}
