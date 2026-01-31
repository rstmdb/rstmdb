//! Snapshot storage.

use crate::error::StorageError;
use parking_lot::RwLock;
use rstmdb_core::instance::InstanceSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

/// Snapshot policy configuration.
#[derive(Debug, Clone)]
pub enum SnapshotPolicy {
    /// Never automatically snapshot.
    Never,
    /// Snapshot after N events.
    EveryNEvents(u64),
    /// Snapshot after N bytes of WAL.
    EveryNBytes(u64),
}

impl Default for SnapshotPolicy {
    fn default() -> Self {
        Self::EveryNEvents(1000)
    }
}

/// Snapshot metadata stored alongside the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub snapshot_id: String,
    pub instance_id: String,
    pub wal_offset: u64,
    pub created_at: i64,
    pub size_bytes: u64,
    pub checksum: String,
}

/// Snapshot store for persisting instance snapshots.
pub struct SnapshotStore {
    dir: PathBuf,
    /// In-memory index of snapshots by instance_id -> latest snapshot_id.
    index: RwLock<HashMap<String, SnapshotMeta>>,
}

impl SnapshotStore {
    /// Opens or creates a snapshot store at the given directory.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;

        let store = Self {
            dir,
            index: RwLock::new(HashMap::new()),
        };

        // Load existing snapshot index
        store.load_index()?;

        Ok(store)
    }

    /// Loads the snapshot index from disk.
    fn load_index(&self) -> Result<(), StorageError> {
        let index_path = self.dir.join("index.json");
        if !index_path.exists() {
            return Ok(());
        }

        let file = File::open(&index_path)?;
        let reader = BufReader::new(file);
        let index: HashMap<String, SnapshotMeta> = serde_json::from_reader(reader)?;
        *self.index.write() = index;

        Ok(())
    }

    /// Saves the snapshot index to disk.
    fn save_index(&self) -> Result<(), StorageError> {
        let index_path = self.dir.join("index.json");
        let file = File::create(&index_path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, &*self.index.read())?;
        Ok(())
    }

    /// Creates a snapshot for an instance.
    pub fn create_snapshot(
        &self,
        snapshot: &InstanceSnapshot,
    ) -> Result<SnapshotMeta, StorageError> {
        // Serialize snapshot
        let data = serde_json::to_vec_pretty(snapshot)?;
        let checksum = format!("{:08x}", crc32c::crc32c(&data));

        // Write snapshot file
        let snapshot_path = self.snapshot_path(&snapshot.snapshot_id);
        let mut file = File::create(&snapshot_path)?;
        file.write_all(&data)?;
        file.sync_all()?;

        // Create metadata
        let meta = SnapshotMeta {
            snapshot_id: snapshot.snapshot_id.clone(),
            instance_id: snapshot.instance_id.clone(),
            wal_offset: snapshot.wal_offset,
            created_at: snapshot.created_at,
            size_bytes: data.len() as u64,
            checksum,
        };

        // Update index
        {
            let mut index = self.index.write();
            index.insert(snapshot.instance_id.clone(), meta.clone());
        }
        self.save_index()?;

        tracing::info!(
            "Created snapshot {} for instance {} at WAL offset {}",
            snapshot.snapshot_id,
            snapshot.instance_id,
            snapshot.wal_offset
        );

        Ok(meta)
    }

    /// Loads a snapshot by ID.
    pub fn load_snapshot(&self, snapshot_id: &str) -> Result<InstanceSnapshot, StorageError> {
        let snapshot_path = self.snapshot_path(snapshot_id);
        if !snapshot_path.exists() {
            return Err(StorageError::SnapshotNotFound(snapshot_id.to_string()));
        }

        let mut file = File::open(&snapshot_path)?;
        let mut data = Vec::new();
        file.read_to_end(&mut data)?;

        // Verify checksum
        let index = self.index.read();
        if let Some(meta) = index.values().find(|m| m.snapshot_id == snapshot_id) {
            let actual_checksum = format!("{:08x}", crc32c::crc32c(&data));
            if actual_checksum != meta.checksum {
                return Err(StorageError::Corruption(format!(
                    "snapshot {} checksum mismatch",
                    snapshot_id
                )));
            }
        }
        drop(index);

        let snapshot: InstanceSnapshot = serde_json::from_slice(&data)?;
        Ok(snapshot)
    }

    /// Gets the latest snapshot for an instance.
    pub fn get_latest_snapshot(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceSnapshot>, StorageError> {
        let snapshot_id = {
            let index = self.index.read();
            index.get(instance_id).map(|m| m.snapshot_id.clone())
        };

        if let Some(sid) = snapshot_id {
            let snapshot = self.load_snapshot(&sid)?;
            Ok(Some(snapshot))
        } else {
            Ok(None)
        }
    }

    /// Gets snapshot metadata for an instance.
    pub fn get_snapshot_meta(&self, instance_id: &str) -> Option<SnapshotMeta> {
        self.index.read().get(instance_id).cloned()
    }

    /// Lists all snapshot metadata.
    pub fn list_snapshots(&self) -> Vec<SnapshotMeta> {
        self.index.read().values().cloned().collect()
    }

    /// Deletes a snapshot.
    pub fn delete_snapshot(&self, snapshot_id: &str) -> Result<(), StorageError> {
        let snapshot_path = self.snapshot_path(snapshot_id);
        if snapshot_path.exists() {
            fs::remove_file(&snapshot_path)?;
        }

        // Update index
        {
            let mut index = self.index.write();
            index.retain(|_, meta| meta.snapshot_id != snapshot_id);
        }
        self.save_index()?;

        Ok(())
    }

    /// Returns the minimum WAL offset across all snapshots.
    ///
    /// This is the oldest point in the WAL that is still needed for recovery.
    /// WAL segments before this offset can be safely deleted.
    ///
    /// Returns None if there are no snapshots.
    pub fn min_wal_offset(&self) -> Option<u64> {
        let index = self.index.read();
        index.values().map(|meta| meta.wal_offset).min()
    }

    /// Returns the number of snapshots stored.
    pub fn snapshot_count(&self) -> usize {
        self.index.read().len()
    }

    /// Returns instance IDs that don't have a snapshot.
    pub fn instances_without_snapshots(&self, all_instance_ids: &[String]) -> Vec<String> {
        let index = self.index.read();
        all_instance_ids
            .iter()
            .filter(|id| !index.contains_key(*id))
            .cloned()
            .collect()
    }

    fn snapshot_path(&self, snapshot_id: &str) -> PathBuf {
        self.dir.join(format!("{}.snap", snapshot_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstmdb_core::instance::Instance;
    use serde_json::json;
    use tempfile::TempDir;

    #[test]
    fn test_snapshot_roundtrip() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();

        let instance = Instance::new("i-1", "order", 1, "paid", json!({"amount": 100}), 5);
        let snapshot = InstanceSnapshot::from_instance(&instance, "snap-1");

        let meta = store.create_snapshot(&snapshot).unwrap();
        assert_eq!(meta.instance_id, "i-1");
        assert_eq!(meta.wal_offset, 5);

        let loaded = store.load_snapshot("snap-1").unwrap();
        assert_eq!(loaded.instance_id, "i-1");
        assert_eq!(loaded.state, "paid");
    }

    #[test]
    fn test_latest_snapshot() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();

        // Create multiple snapshots for same instance
        for i in 1..=3 {
            let instance = Instance::new(
                "i-1",
                "order",
                1,
                format!("state-{}", i),
                json!({}),
                i as u64,
            );
            let snapshot = InstanceSnapshot::from_instance(&instance, format!("snap-{}", i));
            store.create_snapshot(&snapshot).unwrap();
        }

        let latest = store.get_latest_snapshot("i-1").unwrap().unwrap();
        assert_eq!(latest.snapshot_id, "snap-3");
        assert_eq!(latest.state, "state-3");
    }

    #[test]
    fn test_min_wal_offset() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();

        // No snapshots
        assert!(store.min_wal_offset().is_none());

        // Create snapshots with different offsets
        for (i, offset) in [(1, 100u64), (2, 50), (3, 200)].iter() {
            let instance =
                Instance::new(format!("i-{}", i), "order", 1, "state", json!({}), *offset);
            let snapshot = InstanceSnapshot::from_instance(&instance, format!("snap-{}", i));
            store.create_snapshot(&snapshot).unwrap();
        }

        // Min should be 50
        assert_eq!(store.min_wal_offset(), Some(50));
    }

    #[test]
    fn test_snapshot_count() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();

        assert_eq!(store.snapshot_count(), 0);

        for i in 1..=3 {
            let instance = Instance::new(format!("i-{}", i), "order", 1, "state", json!({}), i);
            let snapshot = InstanceSnapshot::from_instance(&instance, format!("snap-{}", i));
            store.create_snapshot(&snapshot).unwrap();
        }

        assert_eq!(store.snapshot_count(), 3);
    }

    #[test]
    fn test_instances_without_snapshots() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();

        // Create snapshot for i-1 only
        let instance = Instance::new("i-1", "order", 1, "state", json!({}), 1);
        let snapshot = InstanceSnapshot::from_instance(&instance, "snap-1");
        store.create_snapshot(&snapshot).unwrap();

        let all_instances = vec!["i-1".to_string(), "i-2".to_string(), "i-3".to_string()];
        let without = store.instances_without_snapshots(&all_instances);

        assert_eq!(without.len(), 2);
        assert!(without.contains(&"i-2".to_string()));
        assert!(without.contains(&"i-3".to_string()));
    }
}
