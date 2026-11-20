//! Snapshot storage.

use crate::error::StorageError;
use parking_lot::{Mutex, RwLock};
use rstmdb_core::instance::InstanceSnapshot;
use rstmdb_core::{MachineRecord, RecoveryState};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{BufReader, Read, Write};
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
    /// Serializes mutations so index.json / machines.json writes never race.
    write_lock: Mutex<()>,
}

const INDEX_FILE: &str = "index.json";
const MACHINES_FILE: &str = "machines.json";

/// On-disk `machines.json`: definitions plus the WAL head they reflect.
#[derive(Debug, Serialize, Deserialize)]
struct MachinesFile {
    wal_offset: u64,
    machines: Vec<MachineRecord>,
}

/// Accepts the current format and the older bare array.
#[derive(Deserialize)]
#[serde(untagged)]
enum MachinesOnDisk {
    Current(MachinesFile),
    Legacy(Vec<MachineRecord>),
}

/// Removes `path`; a missing file is not an error.
fn remove_if_exists(path: &Path) -> Result<(), StorageError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Writes `data` to `path` atomically: temp file, fsync, rename, fsync dir.
/// A crash leaves either the old or the new file, never a torn one.
fn atomic_write(path: &Path, data: &[u8]) -> Result<(), StorageError> {
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{}.tmp-{}", file_name, uuid::Uuid::new_v4()));
    {
        let mut file = File::create(&tmp)?;
        file.write_all(data)?;
        file.sync_all()?;
    }
    if let Err(e) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    sync_dir(dir);
    Ok(())
}

#[cfg(unix)]
fn sync_dir(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

#[cfg(not(unix))]
fn sync_dir(_dir: &Path) {}

impl SnapshotStore {
    /// Opens or creates a snapshot store at the given directory.
    ///
    /// # Examples
    ///
    /// ```
    /// use rstmdb_storage::SnapshotStore;
    ///
    /// let dir = tempfile::TempDir::new().unwrap();
    /// let store = SnapshotStore::open(dir.path()).unwrap();
    /// assert_eq!(store.snapshot_count(), 0);
    /// ```
    pub fn open(dir: impl AsRef<Path>) -> Result<Self, StorageError> {
        let dir = dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;

        let store = Self {
            dir,
            index: RwLock::new(HashMap::new()),
            write_lock: Mutex::new(()),
        };

        // Load existing snapshot index
        store.load_index()?;

        Ok(store)
    }

    /// Loads the snapshot index from disk.
    fn load_index(&self) -> Result<(), StorageError> {
        let index_path = self.dir.join(INDEX_FILE);
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
        let data = serde_json::to_vec_pretty(&*self.index.read())?;
        atomic_write(&self.dir.join(INDEX_FILE), &data)
    }

    /// Creates a snapshot for an instance and persists it to disk.
    ///
    /// # Examples
    ///
    /// ```
    /// use rstmdb_storage::SnapshotStore;
    /// use rstmdb_core::instance::InstanceSnapshot;
    /// use rstmdb_core::Instance;
    /// use serde_json::json;
    ///
    /// let dir = tempfile::TempDir::new().unwrap();
    /// let store = SnapshotStore::open(dir.path()).unwrap();
    ///
    /// let instance = Instance::new("i1", "order", 1, "created", json!({}), 0);
    /// let snapshot = InstanceSnapshot::from_instance(&instance, "snap-001");
    /// let meta = store.create_snapshot(&snapshot).unwrap();
    ///
    /// assert_eq!(meta.instance_id, "i1");
    /// assert_eq!(store.snapshot_count(), 1);
    /// ```
    pub fn create_snapshot(
        &self,
        snapshot: &InstanceSnapshot,
    ) -> Result<SnapshotMeta, StorageError> {
        // Serialize snapshot
        let data = serde_json::to_vec_pretty(snapshot)?;
        let checksum = format!("{:08x}", crc32c::crc32c(&data));

        let _guard = self.write_lock.lock();

        // Write snapshot file
        let snapshot_path = self.snapshot_path(&snapshot.snapshot_id);
        atomic_write(&snapshot_path, &data)?;

        // Create metadata
        let meta = SnapshotMeta {
            snapshot_id: snapshot.snapshot_id.clone(),
            instance_id: snapshot.instance_id.clone(),
            wal_offset: snapshot.wal_offset,
            created_at: snapshot.created_at,
            size_bytes: data.len() as u64,
            checksum,
        };

        // Update index; the superseded snapshot file is removed only after
        // the new index is durable.
        let previous = {
            let mut index = self.index.write();
            index.insert(snapshot.instance_id.clone(), meta.clone())
        };
        self.save_index()?;
        if let Some(prev) = previous {
            if prev.snapshot_id != meta.snapshot_id {
                let _ = fs::remove_file(self.snapshot_path(&prev.snapshot_id));
            }
        }

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
        let _guard = self.write_lock.lock();
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

    /// Removes every snapshot and machines.json (FLUSH_ALL).
    ///
    /// The empty index is written first, so once this returns Ok — or crashes
    /// after that point — no snapshot is reachable; a leftover `.snap` file is
    /// only garbage.
    pub fn clear(&self) -> Result<(), StorageError> {
        let _guard = self.write_lock.lock();
        let removed = std::mem::take(&mut *self.index.write());
        if let Err(e) = self.save_index() {
            *self.index.write() = removed;
            return Err(e);
        }
        remove_if_exists(&self.dir.join(MACHINES_FILE))?;
        for meta in removed.values() {
            remove_if_exists(&self.snapshot_path(&meta.snapshot_id))?;
        }
        sync_dir(&self.dir);
        Ok(())
    }

    /// Drops state older than a FLUSH_ALL marker at `wal_offset`: snapshots
    /// and a machines.json whose offset is at or before it (a WAL head taken
    /// right before the flush equals the marker offset). Returns the number of
    /// snapshots discarded. Idempotent.
    pub fn discard_before(&self, wal_offset: u64) -> Result<usize, StorageError> {
        let _guard = self.write_lock.lock();
        let stale: Vec<SnapshotMeta> = self
            .index
            .read()
            .values()
            .filter(|meta| meta.wal_offset <= wal_offset)
            .cloned()
            .collect();
        if !stale.is_empty() {
            {
                let mut index = self.index.write();
                for meta in &stale {
                    index.remove(&meta.instance_id);
                }
            }
            self.save_index()?;
            for meta in &stale {
                remove_if_exists(&self.snapshot_path(&meta.snapshot_id))?;
            }
        }
        if let Some(file) = self.read_machines_file()? {
            if file.wal_offset <= wal_offset {
                remove_if_exists(&self.dir.join(MACHINES_FILE))?;
            }
        }
        sync_dir(&self.dir);
        Ok(stale.len())
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

    /// Persists all machine definitions (atomically replaces machines.json).
    ///
    /// `wal_offset` is the WAL head the definitions reflect; recovery ignores
    /// the file if a FLUSH_ALL marker was written after it.
    ///
    /// Must be called before WAL compaction so definitions whose PUT_MACHINE
    /// entries are compacted away can still be recovered.
    pub fn save_machines(
        &self,
        machines: &[MachineRecord],
        wal_offset: u64,
    ) -> Result<(), StorageError> {
        let _guard = self.write_lock.lock();
        let file = MachinesFile {
            wal_offset,
            machines: machines.to_vec(),
        };
        let data = serde_json::to_vec_pretty(&file)?;
        atomic_write(&self.dir.join(MACHINES_FILE), &data)
    }

    /// Loads persisted machine definitions (empty if none were saved).
    pub fn load_machines(&self) -> Result<Vec<MachineRecord>, StorageError> {
        Ok(self
            .read_machines_file()?
            .map(|f| f.machines)
            .unwrap_or_default())
    }

    fn read_machines_file(&self) -> Result<Option<MachinesFile>, StorageError> {
        let path = self.dir.join(MACHINES_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let file = File::open(&path)?;
        Ok(Some(match serde_json::from_reader(BufReader::new(file))? {
            MachinesOnDisk::Current(f) => f,
            MachinesOnDisk::Legacy(machines) => MachinesFile {
                wal_offset: 0,
                machines,
            },
        }))
    }

    /// Loads everything needed to recover the engine: persisted machines and
    /// the latest snapshot of every indexed instance.
    ///
    /// Fails on any missing or corrupt snapshot rather than starting with
    /// silently missing instances.
    pub fn load_recovery_state(&self) -> Result<RecoveryState, StorageError> {
        let (machines, machines_wal_offset) = self
            .read_machines_file()?
            .map(|f| (f.machines, f.wal_offset))
            .unwrap_or_default();
        let metas: Vec<SnapshotMeta> = self.index.read().values().cloned().collect();
        let mut snapshots = Vec::with_capacity(metas.len());
        for meta in metas {
            let snapshot = self.load_snapshot(&meta.snapshot_id)?;
            if snapshot.instance_id != meta.instance_id {
                return Err(StorageError::Corruption(format!(
                    "snapshot {} belongs to {}, indexed under {}",
                    meta.snapshot_id, snapshot.instance_id, meta.instance_id
                )));
            }
            snapshots.push(snapshot);
        }
        Ok(RecoveryState {
            machines,
            snapshots,
            machines_wal_offset,
        })
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
    fn test_superseded_snapshot_file_removed() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();
        for i in 1..=2 {
            let instance = Instance::new("i-1", "order", 1, "s", json!({}), i);
            store
                .create_snapshot(&InstanceSnapshot::from_instance(
                    &instance,
                    format!("snap-{}", i),
                ))
                .unwrap();
        }
        assert!(!dir.path().join("snap-1.snap").exists());
        assert!(dir.path().join("snap-2.snap").exists());
    }

    #[test]
    fn test_recovery_state_roundtrip() {
        let dir = TempDir::new().unwrap();
        {
            let store = SnapshotStore::open(dir.path()).unwrap();
            store
                .save_machines(
                    &[MachineRecord {
                        machine: "order".into(),
                        version: 1,
                        definition: json!({"states": ["a"], "initial": "a", "transitions": []}),
                    }],
                    0,
                )
                .unwrap();
            let mut instance = Instance::new("i-1", "order", 1, "a", json!({}), 9);
            instance.soft_delete(10);
            store
                .create_snapshot(&InstanceSnapshot::from_instance(&instance, "snap-x"))
                .unwrap();
        }
        // Reopen: index and machines come back from disk.
        let store = SnapshotStore::open(dir.path()).unwrap();
        let state = store.load_recovery_state().unwrap();
        assert_eq!(state.machines.len(), 1);
        assert_eq!(state.snapshots.len(), 1);
        assert!(state.snapshots[0].deleted);
    }

    fn record(name: &str) -> MachineRecord {
        MachineRecord {
            machine: name.into(),
            version: 1,
            definition: json!({"states": ["a"], "initial": "a", "transitions": []}),
        }
    }

    #[test]
    fn test_machines_file_records_wal_offset() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();
        store.save_machines(&[record("order")], 77).unwrap();

        let state = SnapshotStore::open(dir.path())
            .unwrap()
            .load_recovery_state()
            .unwrap();
        assert_eq!(state.machines.len(), 1);
        assert_eq!(state.machines_wal_offset, 77);
    }

    #[test]
    fn test_legacy_machines_array_loads_with_offset_zero() {
        let dir = TempDir::new().unwrap();
        let legacy = serde_json::to_vec(&vec![record("order")]).unwrap();
        fs::write(dir.path().join(MACHINES_FILE), legacy).unwrap();

        let state = SnapshotStore::open(dir.path())
            .unwrap()
            .load_recovery_state()
            .unwrap();
        assert_eq!(state.machines.len(), 1);
        assert_eq!(state.machines_wal_offset, 0);
    }

    #[test]
    fn test_clear_removes_everything_durably() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();
        store.save_machines(&[record("order")], 5).unwrap();
        for (id, off) in [("i-1", 3), ("i-2", 9)] {
            let instance = Instance::new(id, "order", 1, "a", json!({}), off);
            store
                .create_snapshot(&InstanceSnapshot::from_instance(
                    &instance,
                    format!("snap-{id}"),
                ))
                .unwrap();
        }

        store.clear().unwrap();
        assert_eq!(store.snapshot_count(), 0);
        assert!(!dir.path().join(MACHINES_FILE).exists());
        assert!(!dir.path().join("snap-i-1.snap").exists());
        assert!(!dir.path().join("snap-i-2.snap").exists());

        let reopened = SnapshotStore::open(dir.path()).unwrap();
        let state = reopened.load_recovery_state().unwrap();
        assert!(state.machines.is_empty());
        assert!(state.snapshots.is_empty());
    }

    #[test]
    fn test_discard_before_keeps_newer_state() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();
        for (id, off) in [("old", 3), ("new", 20)] {
            let instance = Instance::new(id, "order", 1, "a", json!({}), off);
            store
                .create_snapshot(&InstanceSnapshot::from_instance(
                    &instance,
                    format!("snap-{id}"),
                ))
                .unwrap();
        }
        store.save_machines(&[record("order")], 25).unwrap();

        assert_eq!(store.discard_before(10).unwrap(), 1);
        assert!(store.get_snapshot_meta("old").is_none());
        assert!(store.get_snapshot_meta("new").is_some());
        assert!(!dir.path().join("snap-old.snap").exists());
        assert!(
            dir.path().join(MACHINES_FILE).exists(),
            "machines at 25 >= 10 must stay"
        );

        // Idempotent, and a machines.json written at the marker offset (head
        // captured right before the flush) is removed.
        store.save_machines(&[record("order")], 10).unwrap();
        assert_eq!(store.discard_before(10).unwrap(), 0);
        assert!(!dir.path().join(MACHINES_FILE).exists());

        let reopened = SnapshotStore::open(dir.path()).unwrap();
        assert_eq!(reopened.snapshot_count(), 1);
    }

    #[test]
    fn test_recovery_state_fails_on_corrupt_snapshot() {
        let dir = TempDir::new().unwrap();
        let store = SnapshotStore::open(dir.path()).unwrap();
        let instance = Instance::new("i-1", "order", 1, "a", json!({}), 9);
        store
            .create_snapshot(&InstanceSnapshot::from_instance(&instance, "snap-1"))
            .unwrap();
        fs::write(dir.path().join("snap-1.snap"), b"{garbage").unwrap();
        assert!(store.load_recovery_state().is_err());
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
