//! Idempotency index for deduplication.

use crate::error::StorageError;
use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Cached idempotency result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyEntry {
    /// The idempotency key.
    pub key: String,

    /// Instance ID (if applicable).
    pub instance_id: Option<String>,

    /// Operation type.
    pub operation: String,

    /// WAL offset of the result.
    pub wal_offset: u64,

    /// Serialized result.
    pub result: serde_json::Value,

    /// Creation timestamp (Unix millis).
    pub created_at: i64,
}

/// In-memory idempotency index with persistence.
pub struct IdempotencyIndex {
    /// Index: (instance_id, idempotency_key) -> entry.
    /// For operations without instance_id, use empty string.
    entries: RwLock<HashMap<(String, String), IdempotencyEntry>>,

    /// Retention duration.
    retention: Duration,

    /// Path for persistence.
    persist_path: Option<PathBuf>,

    /// Last cleanup time.
    last_cleanup: RwLock<Instant>,
}

impl IdempotencyIndex {
    /// Creates a new in-memory idempotency index.
    pub fn new(retention: Duration) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            retention,
            persist_path: None,
            last_cleanup: RwLock::new(Instant::now()),
        }
    }

    /// Creates an idempotency index with persistence.
    pub fn with_persistence(
        retention: Duration,
        path: impl AsRef<Path>,
    ) -> Result<Self, StorageError> {
        let path = path.as_ref().to_path_buf();
        let index = Self {
            entries: RwLock::new(HashMap::new()),
            retention,
            persist_path: Some(path.clone()),
            last_cleanup: RwLock::new(Instant::now()),
        };

        // Load existing index
        if path.exists() {
            let file = File::open(&path)?;
            let reader = BufReader::new(file);
            let entries: HashMap<(String, String), IdempotencyEntry> =
                serde_json::from_reader(reader)?;
            *index.entries.write() = entries;
        }

        Ok(index)
    }

    /// Looks up an idempotency entry.
    pub fn get(&self, instance_id: Option<&str>, key: &str) -> Option<IdempotencyEntry> {
        let instance_id = instance_id.unwrap_or("");
        self.entries
            .read()
            .get(&(instance_id.to_string(), key.to_string()))
            .cloned()
    }

    /// Stores an idempotency entry.
    pub fn put(&self, entry: IdempotencyEntry) -> Result<(), StorageError> {
        let instance_id = entry.instance_id.clone().unwrap_or_default();
        let key = entry.key.clone();

        {
            let mut entries = self.entries.write();
            entries.insert((instance_id, key), entry);
        }

        // Periodic cleanup
        self.maybe_cleanup()?;

        Ok(())
    }

    /// Checks if an idempotency key exists.
    pub fn contains(&self, instance_id: Option<&str>, key: &str) -> bool {
        let instance_id = instance_id.unwrap_or("");
        self.entries
            .read()
            .contains_key(&(instance_id.to_string(), key.to_string()))
    }

    /// Removes expired entries.
    fn maybe_cleanup(&self) -> Result<(), StorageError> {
        let cleanup_interval = Duration::from_secs(60);

        let should_cleanup = {
            let last = self.last_cleanup.read();
            last.elapsed() > cleanup_interval
        };

        if should_cleanup {
            self.cleanup()?;
            *self.last_cleanup.write() = Instant::now();
        }

        Ok(())
    }

    /// Removes expired entries and persists.
    pub fn cleanup(&self) -> Result<(), StorageError> {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        let retention_ms = self.retention.as_millis() as i64;

        {
            let mut entries = self.entries.write();
            entries.retain(|_, entry| now - entry.created_at < retention_ms);
        }

        self.persist()?;

        Ok(())
    }

    /// Persists the index to disk.
    pub fn persist(&self) -> Result<(), StorageError> {
        if let Some(path) = &self.persist_path {
            let file = File::create(path)?;
            let writer = BufWriter::new(file);
            serde_json::to_writer(writer, &*self.entries.read())?;
        }
        Ok(())
    }

    /// Returns the retention duration.
    pub fn retention(&self) -> Duration {
        self.retention
    }

    /// Returns the number of entries.
    pub fn len(&self) -> usize {
        self.entries.read().len()
    }

    /// Returns true if the index is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.read().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_idempotency_basic() {
        let index = IdempotencyIndex::new(Duration::from_secs(3600));

        let entry = IdempotencyEntry {
            key: "key-1".to_string(),
            instance_id: Some("i-1".to_string()),
            operation: "APPLY_EVENT".to_string(),
            wal_offset: 100,
            result: json!({"success": true}),
            created_at: 0,
        };

        index.put(entry.clone()).unwrap();

        assert!(index.contains(Some("i-1"), "key-1"));
        assert!(!index.contains(Some("i-1"), "key-2"));
        assert!(!index.contains(Some("i-2"), "key-1"));

        let retrieved = index.get(Some("i-1"), "key-1").unwrap();
        assert_eq!(retrieved.wal_offset, 100);
    }

    #[test]
    fn test_idempotency_without_instance() {
        let index = IdempotencyIndex::new(Duration::from_secs(3600));

        let entry = IdempotencyEntry {
            key: "key-1".to_string(),
            instance_id: None,
            operation: "PUT_MACHINE".to_string(),
            wal_offset: 50,
            result: json!({"created": true}),
            created_at: 0,
        };

        index.put(entry).unwrap();

        assert!(index.contains(None, "key-1"));
        let retrieved = index.get(None, "key-1").unwrap();
        assert_eq!(retrieved.wal_offset, 50);
    }
}
