//! Unified storage abstraction.

use crate::error::StorageError;
use crate::index::{IdempotencyEntry, IdempotencyIndex};
use crate::snapshot::{SnapshotMeta, SnapshotPolicy, SnapshotStore};
use parking_lot::RwLock;
use rstmdb_core::definition::MachineDefinition;
use rstmdb_core::instance::{Instance, InstanceSnapshot};
use rstmdb_wal::{Wal, WalConfig, WalEntry, WalOffset};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Storage configuration.
#[derive(Debug, Clone)]
pub struct StorageConfig {
    /// Base directory for all storage.
    pub dir: PathBuf,
    /// WAL configuration.
    pub wal: WalConfig,
    /// Snapshot policy.
    pub snapshot_policy: SnapshotPolicy,
    /// Idempotency key retention.
    pub idempotency_retention: Duration,
}

impl StorageConfig {
    pub fn new(dir: impl AsRef<Path>) -> Self {
        let dir = dir.as_ref().to_path_buf();
        Self {
            wal: WalConfig::new(dir.join("wal")),
            dir,
            snapshot_policy: SnapshotPolicy::default(),
            idempotency_retention: Duration::from_secs(3600 * 24), // 24 hours
        }
    }
}

/// Machine definition storage entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct StoredMachine {
    name: String,
    version: u32,
    definition: serde_json::Value,
    checksum: String,
}

/// Unified storage layer.
pub struct Storage {
    config: StorageConfig,

    /// Write-ahead log.
    wal: Arc<Wal>,

    /// Snapshot store.
    snapshots: SnapshotStore,

    /// Idempotency index.
    idempotency: IdempotencyIndex,

    /// Machine definitions (in-memory, persisted to disk).
    machines: RwLock<HashMap<(String, u32), StoredMachine>>,

    /// Instances (in-memory, persisted via snapshots).
    instances: RwLock<HashMap<String, Instance>>,
}

impl Storage {
    /// Opens or creates storage at the given directory.
    pub fn open(config: StorageConfig) -> Result<Self, StorageError> {
        // Create directories
        fs::create_dir_all(&config.dir)?;
        fs::create_dir_all(config.dir.join("machines"))?;

        // Open WAL
        let wal = Arc::new(Wal::open(config.wal.clone())?);

        // Open snapshot store
        let snapshots = SnapshotStore::open(config.dir.join("snapshots"))?;

        // Open idempotency index
        let idempotency = IdempotencyIndex::with_persistence(
            config.idempotency_retention,
            config.dir.join("idempotency.json"),
        )?;

        let storage = Self {
            config,
            wal,
            snapshots,
            idempotency,
            machines: RwLock::new(HashMap::new()),
            instances: RwLock::new(HashMap::new()),
        };

        // Load machines from disk
        storage.load_machines()?;

        // Recover instances from snapshots + WAL
        storage.recover()?;

        Ok(storage)
    }

    /// Loads machine definitions from disk.
    fn load_machines(&self) -> Result<(), StorageError> {
        let machines_dir = self.config.dir.join("machines");
        if !machines_dir.exists() {
            return Ok(());
        }

        for entry in fs::read_dir(&machines_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().map(|e| e == "json").unwrap_or(false) {
                let file = File::open(&path)?;
                let reader = BufReader::new(file);
                let stored: StoredMachine = serde_json::from_reader(reader)?;
                self.machines
                    .write()
                    .insert((stored.name.clone(), stored.version), stored);
            }
        }

        Ok(())
    }

    /// Recovers instances from snapshots and WAL replay.
    fn recover(&self) -> Result<(), StorageError> {
        // Load all snapshots
        for meta in self.snapshots.list_snapshots() {
            if let Ok(snapshot) = self.snapshots.load_snapshot(&meta.snapshot_id) {
                let instance = snapshot.to_instance();
                self.instances.write().insert(instance.id.clone(), instance);
            }
        }

        // Replay WAL from earliest point
        // In a full implementation, we'd track the minimum snapshot offset
        // and only replay from there
        let earliest = self.wal.earliest_offset().unwrap_or(WalOffset::from_u64(0));
        let entries = self.wal.read_from(earliest, None)?;

        for (_, _, entry) in entries {
            self.apply_wal_entry(&entry)?;
        }

        tracing::info!(
            "Recovery complete: {} machines, {} instances",
            self.machines.read().len(),
            self.instances.read().len()
        );

        Ok(())
    }

    /// Applies a WAL entry during recovery.
    fn apply_wal_entry(&self, entry: &WalEntry) -> Result<(), StorageError> {
        match entry {
            WalEntry::PutMachine {
                machine,
                version,
                definition_hash,
                ..
            } => {
                // Machine definitions are loaded from disk, WAL just confirms
                tracing::debug!(
                    "WAL: PutMachine {} v{} (hash: {})",
                    machine,
                    version,
                    definition_hash
                );
            }
            WalEntry::CreateInstance {
                instance_id,
                machine,
                version,
                initial_state,
                initial_ctx,
                ..
            } => {
                if !self.instances.read().contains_key(instance_id) {
                    let instance = Instance::new(
                        instance_id,
                        machine,
                        *version,
                        initial_state,
                        initial_ctx.clone(),
                        0, // Will be updated by later entries
                    );
                    self.instances.write().insert(instance_id.clone(), instance);
                }
            }
            WalEntry::ApplyEvent {
                instance_id,
                to_state,
                ctx,
                event_id,
                ..
            } => {
                if let Some(instance) = self.instances.write().get_mut(instance_id) {
                    instance.apply_transition(to_state, ctx.clone(), event_id.clone(), 0);
                }
            }
            WalEntry::DeleteInstance { instance_id, .. } => {
                if let Some(instance) = self.instances.write().get_mut(instance_id) {
                    instance.soft_delete(0);
                }
            }
            WalEntry::Snapshot { .. } | WalEntry::Checkpoint { .. } => {
                // Handled separately
            }
        }

        Ok(())
    }

    // =========================================================================
    // Public API
    // =========================================================================

    /// Returns a reference to the WAL.
    pub fn wal(&self) -> &Arc<Wal> {
        &self.wal
    }

    /// Stores a machine definition.
    pub fn put_machine(&self, definition: &MachineDefinition) -> Result<(), StorageError> {
        let stored = StoredMachine {
            name: definition.name.clone(),
            version: definition.version,
            definition: definition.to_json(),
            checksum: definition.checksum.clone(),
        };

        // Persist to disk
        let path = self
            .config
            .dir
            .join("machines")
            .join(format!("{}_{}.json", stored.name, stored.version));
        let file = File::create(&path)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, &stored)?;

        // Update in-memory
        self.machines
            .write()
            .insert((stored.name.clone(), stored.version), stored);

        Ok(())
    }

    /// Gets a machine definition.
    pub fn get_machine(&self, name: &str, version: u32) -> Option<serde_json::Value> {
        self.machines
            .read()
            .get(&(name.to_string(), version))
            .map(|m| m.definition.clone())
    }

    /// Stores an instance.
    pub fn put_instance(&self, instance: &Instance) -> Result<(), StorageError> {
        self.instances
            .write()
            .insert(instance.id.clone(), instance.clone());
        Ok(())
    }

    /// Gets an instance.
    pub fn get_instance(&self, instance_id: &str) -> Option<Instance> {
        self.instances.read().get(instance_id).cloned()
    }

    /// Creates a snapshot for an instance.
    pub fn create_snapshot(
        &self,
        instance_id: &str,
        snapshot_id: &str,
    ) -> Result<SnapshotMeta, StorageError> {
        let instance = self
            .instances
            .read()
            .get(instance_id)
            .cloned()
            .ok_or_else(|| StorageError::InstanceNotFound(instance_id.to_string()))?;

        let snapshot = InstanceSnapshot::from_instance(&instance, snapshot_id);
        let meta = self.snapshots.create_snapshot(&snapshot)?;

        Ok(meta)
    }

    /// Gets the latest snapshot for an instance.
    pub fn get_latest_snapshot(
        &self,
        instance_id: &str,
    ) -> Result<Option<InstanceSnapshot>, StorageError> {
        self.snapshots.get_latest_snapshot(instance_id)
    }

    /// Checks idempotency.
    pub fn check_idempotency(
        &self,
        instance_id: Option<&str>,
        key: &str,
    ) -> Option<IdempotencyEntry> {
        self.idempotency.get(instance_id, key)
    }

    /// Records idempotency.
    pub fn record_idempotency(&self, entry: IdempotencyEntry) -> Result<(), StorageError> {
        self.idempotency.put(entry)
    }

    /// Syncs all storage to disk.
    pub fn sync(&self) -> Result<(), StorageError> {
        self.wal.sync()?;
        self.idempotency.persist()?;
        Ok(())
    }

    /// Compacts the WAL by removing segments that are no longer needed.
    ///
    /// A segment is safe to delete if all instances have snapshots with
    /// WAL offsets beyond that segment.
    ///
    /// If `force_snapshot` is true, creates snapshots for any instances
    /// that don't have one before compacting.
    ///
    /// Returns the number of segments compacted.
    pub fn compact(&self, force_snapshot: bool) -> Result<CompactionResult, StorageError> {
        let instance_ids: Vec<String> = self.instances.read().keys().cloned().collect();

        if instance_ids.is_empty() {
            return Ok(CompactionResult {
                snapshots_created: 0,
                segments_deleted: 0,
                bytes_reclaimed: 0,
            });
        }

        let mut snapshots_created = 0;

        // Check which instances need snapshots
        let instances_needing_snapshots = self.snapshots.instances_without_snapshots(&instance_ids);

        if !instances_needing_snapshots.is_empty() {
            if force_snapshot {
                // Create snapshots for instances without them
                for instance_id in &instances_needing_snapshots {
                    let snapshot_id = format!("compact-{}-{}", instance_id, uuid::Uuid::new_v4());
                    if self.create_snapshot(instance_id, &snapshot_id).is_ok() {
                        snapshots_created += 1;
                        tracing::debug!("Created compaction snapshot for instance {}", instance_id);
                    }
                }
            } else {
                // Can't compact if some instances don't have snapshots
                tracing::debug!(
                    "Cannot compact: {} instances without snapshots",
                    instances_needing_snapshots.len()
                );
                return Ok(CompactionResult {
                    snapshots_created: 0,
                    segments_deleted: 0,
                    bytes_reclaimed: 0,
                });
            }
        }

        // Find the minimum WAL offset across all snapshots
        let min_offset = match self.snapshots.min_wal_offset() {
            Some(offset) => offset,
            None => {
                return Ok(CompactionResult {
                    snapshots_created,
                    segments_deleted: 0,
                    bytes_reclaimed: 0,
                })
            }
        };

        // Calculate size before compaction
        let size_before = self.wal.total_size();

        // Compact WAL segments before the minimum offset
        let segments_deleted = self.wal.compact_before(WalOffset::from_u64(min_offset))?;

        // Calculate bytes reclaimed
        let size_after = self.wal.total_size();
        let bytes_reclaimed = size_before.saturating_sub(size_after);

        if segments_deleted > 0 {
            tracing::info!(
                "Compacted {} WAL segments, reclaimed {} bytes",
                segments_deleted,
                bytes_reclaimed
            );
        }

        Ok(CompactionResult {
            snapshots_created,
            segments_deleted,
            bytes_reclaimed,
        })
    }

    /// Creates snapshots for all instances and then compacts.
    ///
    /// This is useful for periodic maintenance.
    pub fn snapshot_all_and_compact(&self) -> Result<CompactionResult, StorageError> {
        let mut snapshots_created = 0;

        // Create snapshots for all instances
        for (instance_id, _) in self.instances.read().iter() {
            let snapshot_id = format!("periodic-{}-{}", instance_id, uuid::Uuid::new_v4());
            if self.create_snapshot(instance_id, &snapshot_id).is_ok() {
                snapshots_created += 1;
            }
        }

        // Now compact
        let mut result = self.compact(false)?;
        result.snapshots_created += snapshots_created;

        Ok(result)
    }

    /// Returns the current WAL size in bytes.
    pub fn wal_size(&self) -> u64 {
        self.wal.total_size()
    }

    /// Returns the number of WAL segments.
    pub fn wal_segment_count(&self) -> usize {
        self.wal.segment_ids().len()
    }
}

/// Result of a compaction operation.
#[derive(Debug, Clone, Default)]
pub struct CompactionResult {
    /// Number of snapshots created during compaction.
    pub snapshots_created: usize,
    /// Number of WAL segments deleted.
    pub segments_deleted: usize,
    /// Bytes reclaimed from deleted segments.
    pub bytes_reclaimed: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstmdb_wal::FsyncPolicy;
    use serde_json::json;
    use tempfile::TempDir;

    fn test_config(dir: &Path) -> StorageConfig {
        let mut config = StorageConfig::new(dir);
        config.wal = config.wal.with_fsync_policy(FsyncPolicy::EveryWrite);
        config
    }

    #[test]
    fn test_storage_roundtrip() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::open(test_config(dir.path())).unwrap();

        // Store machine
        let def = MachineDefinition::from_json(
            "order",
            1,
            &json!({
                "states": ["created", "paid"],
                "initial": "created",
                "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
            }),
        )
        .unwrap();
        storage.put_machine(&def).unwrap();

        // Store instance
        let instance = Instance::new("i-1", "order", 1, "created", json!({}), 0);
        storage.put_instance(&instance).unwrap();

        // Verify
        assert!(storage.get_machine("order", 1).is_some());
        assert!(storage.get_instance("i-1").is_some());
    }

    #[test]
    fn test_snapshot_creation() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::open(test_config(dir.path())).unwrap();

        let instance = Instance::new("i-1", "order", 1, "paid", json!({"amount": 100}), 5);
        storage.put_instance(&instance).unwrap();

        let meta = storage.create_snapshot("i-1", "snap-1").unwrap();
        assert_eq!(meta.instance_id, "i-1");

        let snapshot = storage.get_latest_snapshot("i-1").unwrap().unwrap();
        assert_eq!(snapshot.state, "paid");
    }

    #[test]
    fn test_compaction_no_instances() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::open(test_config(dir.path())).unwrap();

        let result = storage.compact(false).unwrap();
        assert_eq!(result.segments_deleted, 0);
        assert_eq!(result.snapshots_created, 0);
    }

    #[test]
    fn test_compaction_without_snapshots() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::open(test_config(dir.path())).unwrap();

        // Add instance but no snapshot
        let instance = Instance::new("i-1", "order", 1, "created", json!({}), 0);
        storage.put_instance(&instance).unwrap();

        // Without force_snapshot, should not compact
        let result = storage.compact(false).unwrap();
        assert_eq!(result.segments_deleted, 0);
    }

    #[test]
    fn test_compaction_with_force_snapshot() {
        let dir = TempDir::new().unwrap();
        let mut config = test_config(dir.path());
        config.wal = config.wal.with_segment_size(512); // Small segments
        let storage = Storage::open(config).unwrap();

        // Add instance
        let instance = Instance::new("i-1", "order", 1, "created", json!({}), 100);
        storage.put_instance(&instance).unwrap();

        // Force snapshot during compaction
        let result = storage.compact(true).unwrap();
        assert_eq!(result.snapshots_created, 1);
    }

    #[test]
    fn test_wal_size_methods() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::open(test_config(dir.path())).unwrap();

        assert!(storage.wal_size() > 0 || storage.wal_segment_count() > 0);
    }

    #[test]
    fn test_snapshot_all_and_compact() {
        let dir = TempDir::new().unwrap();
        let storage = Storage::open(test_config(dir.path())).unwrap();

        // Add multiple instances
        for i in 1..=3 {
            let instance = Instance::new(
                format!("i-{}", i),
                "order",
                1,
                "created",
                json!({}),
                i as u64,
            );
            storage.put_instance(&instance).unwrap();
        }

        let result = storage.snapshot_all_and_compact().unwrap();
        assert_eq!(result.snapshots_created, 3);
    }
}
