//! Automatic compaction manager.

use crate::config::CompactionConfig;
use rstmdb_core::instance::InstanceSnapshot;
use rstmdb_core::StateMachineEngine;
use rstmdb_storage::{SnapshotStore, StorageError};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Which instances a snapshot pass writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotMode {
    /// Instances with no snapshot or whose state advanced since the last one.
    Changed,
    /// Only instances that have no snapshot yet.
    MissingOnly,
}

/// Outcome of [`snapshot_and_compact`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SnapshotPass {
    pub snapshots_created: usize,
    pub segments_deleted: usize,
}

/// Persists durable state, then deletes WAL segments it fully covers.
///
/// Order matters for crash safety:
/// 1. all machine definitions are written to the snapshot store,
/// 2. instances are snapshotted — soft-deleted ones as tombstones, each with
///    its idempotency cache,
/// 3. only if every write succeeded, segments entirely before both the minimum
///    snapshot offset and the WAL head at the start of the pass are deleted.
///
/// Any failure aborts the pass before step 3, so nothing that recovery needs
/// is ever removed from the WAL.
pub fn snapshot_and_compact(
    engine: &StateMachineEngine,
    store: &SnapshotStore,
    mode: SnapshotMode,
) -> Result<SnapshotPass, StorageError> {
    snapshot_and_compact_with(engine, store, mode, || {})
}

/// [`snapshot_and_compact`] with a hook run after machines are persisted and
/// before instances are snapshotted, so tests can write to the WAL mid-pass.
fn snapshot_and_compact_with(
    engine: &StateMachineEngine,
    store: &SnapshotStore,
    mode: SnapshotMode,
    after_machines: impl FnOnce(),
) -> Result<SnapshotPass, StorageError> {
    let mut pass = SnapshotPass::default();

    // WAL head before anything is persisted. Entries appended during the pass
    // (a PUT_MACHINE after machines.json is written, a CREATE_INSTANCE the
    // instance scan already passed) are in no snapshot, so they must survive
    // even if every snapshot taken below lands in a later segment.
    let head = engine.wal().latest_offset().map(|o| o.as_u64());

    store.save_machines(&engine.machine_records()?, head.unwrap_or(0))?;
    after_machines();

    for (instance, idempotency) in engine.instances_for_snapshot() {
        let needs_snapshot = match (store.get_snapshot_meta(&instance.id), mode) {
            (None, _) => true,
            (Some(meta), SnapshotMode::Changed) => instance.last_wal_offset > meta.wal_offset,
            (Some(_), SnapshotMode::MissingOnly) => false,
        };
        if needs_snapshot {
            let snapshot_id = format!("snap-{}", uuid::Uuid::new_v4());
            let snapshot = InstanceSnapshot::from_instance(&instance, snapshot_id)
                .with_idempotency(idempotency);
            store.create_snapshot(&snapshot)?;
            pass.snapshots_created += 1;
        }
    }

    if let Some(min_offset) = store.min_wal_offset() {
        let before = head.map_or(min_offset, |h| h.min(min_offset));
        pass.segments_deleted = engine
            .wal()
            .compact_before(rstmdb_wal::WalOffset::from_u64(before))?;
    }

    Ok(pass)
}

/// Manages automatic WAL compaction.
pub struct CompactionManager {
    engine: Arc<StateMachineEngine>,
    snapshot_store: Arc<SnapshotStore>,
    config: CompactionConfig,
    events_since_compact: AtomicU64,
    last_compact: parking_lot::Mutex<Instant>,
    shutdown: AtomicBool,
    notify: Notify,
    compaction_gate: std::sync::OnceLock<Arc<parking_lot::Mutex<()>>>,
}

impl CompactionManager {
    /// Creates a new compaction manager.
    pub fn new(
        engine: Arc<StateMachineEngine>,
        snapshot_store: Arc<SnapshotStore>,
        config: CompactionConfig,
    ) -> Self {
        Self {
            engine,
            snapshot_store,
            config,
            events_since_compact: AtomicU64::new(0),
            last_compact: parking_lot::Mutex::new(Instant::now()),
            shutdown: AtomicBool::new(false),
            notify: Notify::new(),
            compaction_gate: std::sync::OnceLock::new(),
        }
    }

    /// Shares a mutex with hot-backup staging so the two never overlap:
    /// staging holds the gate while copying WAL segments/snapshots, and
    /// compaction holds it while mutating them.
    pub fn set_compaction_gate(&self, gate: Arc<parking_lot::Mutex<()>>) {
        let _ = self.compaction_gate.set(gate);
    }

    /// Records that an event occurred (for event-based compaction).
    pub fn record_event(&self) {
        if self.config.is_disabled() {
            return;
        }

        let count = self.events_since_compact.fetch_add(1, Ordering::Relaxed) + 1;

        // Check if we should trigger compaction
        if self.config.events_threshold > 0 && count >= self.config.events_threshold {
            self.notify.notify_one();
        }
    }

    /// Checks if compaction should run based on current conditions.
    fn should_compact(&self) -> bool {
        if self.config.is_disabled() {
            return false;
        }

        // Check minimum interval
        let last = *self.last_compact.lock();
        if last.elapsed() < self.config.min_interval() {
            return false;
        }

        // Check events threshold
        if self.config.events_threshold > 0 {
            let events = self.events_since_compact.load(Ordering::Relaxed);
            if events >= self.config.events_threshold {
                return true;
            }
        }

        // Check size threshold
        if self.config.size_threshold() > 0 {
            let wal_size = self.engine.wal().total_size();
            if wal_size >= self.config.size_threshold() {
                return true;
            }
        }

        false
    }

    /// Runs compaction.
    fn run_compaction(&self) -> CompactionResult {
        // Hold the shared gate (if any) for the duration of the compaction
        // pass so hot-backup staging can't observe a WAL/snapshot mid-mutate.
        let _held = self.compaction_gate.get().map(|g| g.lock());

        let mut result = CompactionResult::default();

        match snapshot_and_compact(&self.engine, &self.snapshot_store, SnapshotMode::Changed) {
            Ok(pass) => {
                result.snapshots_created = pass.snapshots_created;
                result.segments_deleted = pass.segments_deleted;
            }
            Err(e) => {
                // Nothing was deleted from the WAL; retry on the next trigger.
                tracing::error!("Compaction aborted, WAL left intact: {}", e);
            }
        }

        // Reset counters
        self.events_since_compact.store(0, Ordering::Relaxed);
        *self.last_compact.lock() = Instant::now();

        result
    }

    /// Runs the compaction loop (call from a background task).
    pub async fn run(&self) {
        if self.config.is_disabled() {
            tracing::info!("Automatic compaction is disabled");
            return;
        }

        tracing::info!(
            "Compaction manager started (events_threshold={}, size_threshold_mb={})",
            self.config.events_threshold,
            self.config.size_threshold_mb
        );

        let check_interval = Duration::from_secs(10);

        loop {
            // Wait for notification or timeout
            tokio::select! {
                _ = self.notify.notified() => {}
                _ = tokio::time::sleep(check_interval) => {}
            }

            if self.shutdown.load(Ordering::Relaxed) {
                break;
            }

            if self.should_compact() {
                tracing::debug!("Starting automatic compaction");
                let result = self.run_compaction();
                tracing::info!(
                    "Auto-compaction complete: {} snapshots, {} segments deleted",
                    result.snapshots_created,
                    result.segments_deleted
                );
            }
        }

        tracing::info!("Compaction manager stopped");
    }

    /// Test-only wrapper around the private sync compaction pass.
    #[cfg(test)]
    pub fn run_once_for_test(&self) {
        self.run_compaction();
    }

    /// Signals the compaction manager to shut down.
    pub fn shutdown(&self) {
        self.shutdown.store(true, Ordering::Relaxed);
        self.notify.notify_one();
    }

    /// Returns compaction statistics.
    pub fn stats(&self) -> CompactionStats {
        CompactionStats {
            events_since_compact: self.events_since_compact.load(Ordering::Relaxed),
            last_compact: self.last_compact.lock().elapsed(),
        }
    }
}

/// Result of a compaction run.
#[derive(Debug, Default)]
pub struct CompactionResult {
    pub snapshots_created: usize,
    pub segments_deleted: usize,
}

/// Compaction statistics.
#[derive(Debug)]
pub struct CompactionStats {
    pub events_since_compact: u64,
    pub last_compact: Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstmdb_storage::SnapshotStore;
    use rstmdb_wal::{FsyncPolicy, WalConfig};

    fn build_manager(tmp: &std::path::Path) -> CompactionManager {
        let engine = Arc::new(
            StateMachineEngine::new(
                WalConfig::new(tmp.join("wal")).with_fsync_policy(FsyncPolicy::EveryWrite),
            )
            .unwrap(),
        );
        let snapshot_store = Arc::new(SnapshotStore::open(tmp.join("snapshots")).unwrap());
        CompactionManager::new(engine, snapshot_store, CompactionConfig::default())
    }

    fn small_wal(dir: &std::path::Path) -> WalConfig {
        WalConfig::new(dir.join("wal"))
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite)
    }

    /// Regression: before durable recovery, compaction deleted WAL segments
    /// that held machine definitions and instances, and a restart lost them.
    #[test]
    fn compaction_then_restart_preserves_state_and_idempotency() {
        use serde_json::json;

        let tmp = tempfile::TempDir::new().unwrap();
        let definition = json!({
            "states": ["a", "b"],
            "initial": "a",
            "transitions": [
                {"from": "a", "event": "GO", "to": "b"},
                {"from": ["a", "b"], "event": "TOUCH", "to": "a"}
            ]
        });

        let first_pay_offset;
        {
            let engine = StateMachineEngine::new(small_wal(tmp.path())).unwrap();
            let store = SnapshotStore::open(tmp.path().join("snapshots")).unwrap();
            engine.put_machine("m", 1, &definition).unwrap();
            engine
                .create_instance("keep", "m", 1, json!({"x": 1}), Some("create-keep"))
                .unwrap();
            engine
                .create_instance("gone", "m", 1, json!({}), None)
                .unwrap();
            engine
                .create_instance("hot", "m", 1, json!({}), None)
                .unwrap();
            first_pay_offset = engine
                .apply_event(
                    "keep",
                    "GO",
                    json!({"paid": true}),
                    None,
                    None,
                    Some("e-1"),
                    Some("pay"),
                )
                .unwrap()
                .wal_offset;
            // Roll many segments so the early ones become deletable.
            let blob = "x".repeat(1500);
            for i in 0..40 {
                engine
                    .apply_event(
                        "hot",
                        "TOUCH",
                        json!({"blob": blob, "i": i}),
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap();
            }
            // Advance every instance past the early segments.
            engine.delete_instance("gone", None).unwrap();
            engine
                .apply_event("keep", "TOUCH", json!({}), None, None, None, None)
                .unwrap();

            let pass = snapshot_and_compact(&engine, &store, SnapshotMode::Changed).unwrap();
            assert!(
                pass.segments_deleted > 0,
                "test must actually delete segments"
            );
        }

        // Restart: recover from the snapshot store + remaining WAL.
        let store = SnapshotStore::open(tmp.path().join("snapshots")).unwrap();
        let recovery = store.load_recovery_state().unwrap();
        let engine =
            StateMachineEngine::new_with_recovery(small_wal(tmp.path()), recovery).unwrap();

        assert!(
            engine.get_machine("m", 1).is_ok(),
            "machine definition lost"
        );
        let keep = engine.get_instance("keep").unwrap();
        assert_eq!(keep.state, "a");
        assert_eq!(keep.ctx, json!({"x": 1, "paid": true}));
        assert_eq!(engine.get_instance("hot").unwrap().state, "a");
        assert!(
            engine.get_instance("gone").is_err(),
            "deleted instance resurrected"
        );

        let retry = engine
            .apply_event(
                "keep",
                "GO",
                json!({"paid": true}),
                None,
                None,
                None,
                Some("pay"),
            )
            .unwrap();
        assert!(!retry.applied);
        assert_eq!(retry.wal_offset, first_pay_offset);
        assert!(engine
            .create_instance("keep", "m", 1, json!({"x": 1}), Some("create-keep"))
            .is_ok());
    }

    /// FLUSH_ALL, new data, compaction, restart: only the new data exists.
    #[test]
    fn flush_then_compaction_then_restart_keeps_new_data() {
        use serde_json::json;

        let tmp = tempfile::TempDir::new().unwrap();
        let def = json!({"states": ["a"], "initial": "a", "transitions": []});
        {
            let engine = StateMachineEngine::new(small_wal(tmp.path())).unwrap();
            let store = SnapshotStore::open(tmp.path().join("snapshots")).unwrap();
            engine.put_machine("old", 1, &def).unwrap();
            engine
                .create_instance("gone", "old", 1, json!({}), None)
                .unwrap();
            snapshot_and_compact(&engine, &store, SnapshotMode::Changed).unwrap();

            engine.flush_all().unwrap();
            store.clear().unwrap();

            engine.put_machine("new", 1, &def).unwrap();
            engine
                .create_instance("x", "new", 1, json!({"v": 1}), None)
                .unwrap();
            snapshot_and_compact(&engine, &store, SnapshotMode::Changed).unwrap();
        }

        let store = SnapshotStore::open(tmp.path().join("snapshots")).unwrap();
        let engine = StateMachineEngine::new_with_recovery(
            small_wal(tmp.path()),
            store.load_recovery_state().unwrap(),
        )
        .unwrap();
        assert_eq!(engine.list_instance_ids(), vec!["x".to_string()]);
        assert!(engine.get_machine("new", 1).is_ok());
        assert!(engine.get_machine("old", 1).is_err());
    }

    /// Crash after the FlushAll marker but before the store was cleared:
    /// recovery ignores the stale store, discard_before removes it, and a
    /// second restart gives the same result.
    #[test]
    fn crash_before_store_clear_recovers_flushed_state_twice() {
        use serde_json::json;

        let tmp = tempfile::TempDir::new().unwrap();
        let def = json!({"states": ["a"], "initial": "a", "transitions": []});
        {
            let engine = StateMachineEngine::new(small_wal(tmp.path())).unwrap();
            let store = SnapshotStore::open(tmp.path().join("snapshots")).unwrap();
            engine.put_machine("old", 1, &def).unwrap();
            engine
                .create_instance("gone", "old", 1, json!({}), None)
                .unwrap();
            snapshot_and_compact(&engine, &store, SnapshotMode::Changed).unwrap();
            engine.flush_all().unwrap();
            // "Crash": store.clear() never runs.
        }

        for restart in 0..2 {
            let store = SnapshotStore::open(tmp.path().join("snapshots")).unwrap();
            let engine = StateMachineEngine::new_with_recovery(
                small_wal(tmp.path()),
                store.load_recovery_state().unwrap(),
            )
            .unwrap();
            assert_eq!(engine.instance_count(), 0, "restart {restart}");
            assert!(engine.get_machine("old", 1).is_err(), "restart {restart}");

            let f = engine.last_flush_offset().expect("marker must be found");
            store.discard_before(f).unwrap();
            assert_eq!(store.snapshot_count(), 0, "restart {restart}");
            assert!(
                store.load_machines().unwrap().is_empty(),
                "restart {restart}"
            );
        }
    }

    /// Regression: WAL entries written while a pass runs are in neither
    /// machines.json nor any snapshot, so compaction must not delete them even
    /// if every snapshot ends up in a later segment.
    #[test]
    fn compaction_keeps_entries_written_during_the_pass() {
        use serde_json::json;

        let tmp = tempfile::TempDir::new().unwrap();
        let definition = json!({
            "states": ["a"],
            "initial": "a",
            "transitions": [{"from": "a", "event": "TOUCH", "to": "a"}]
        });
        let touch_many = |engine: &StateMachineEngine| {
            let blob = "x".repeat(1500);
            for i in 0..20 {
                engine
                    .apply_event(
                        "hot",
                        "TOUCH",
                        json!({"blob": blob, "i": i}),
                        None,
                        None,
                        None,
                        None,
                    )
                    .unwrap();
            }
        };

        {
            let engine = StateMachineEngine::new(small_wal(tmp.path())).unwrap();
            let store = SnapshotStore::open(tmp.path().join("snapshots")).unwrap();
            engine.put_machine("m", 1, &definition).unwrap();
            engine
                .create_instance("hot", "m", 1, json!({}), None)
                .unwrap();
            // History before the pass, so there is something to compact.
            touch_many(&engine);

            let pass = snapshot_and_compact_with(&engine, &store, SnapshotMode::Changed, || {
                // Lands after machines.json was written...
                engine.put_machine("late", 1, &definition).unwrap();
                // ...and every snapshot taken next is several segments later.
                touch_many(&engine);
            })
            .unwrap();
            assert!(
                pass.segments_deleted > 0,
                "test must actually delete segments"
            );
        }

        let store = SnapshotStore::open(tmp.path().join("snapshots")).unwrap();
        let recovery = store.load_recovery_state().unwrap();
        let engine =
            StateMachineEngine::new_with_recovery(small_wal(tmp.path()), recovery).unwrap();
        assert!(
            engine.get_machine("late", 1).is_ok(),
            "machine registered during compaction was lost"
        );
    }

    #[test]
    fn compaction_respects_gate() {
        use std::sync::atomic::AtomicBool;

        let tmp = tempfile::TempDir::new().unwrap();
        let mgr = build_manager(tmp.path());

        let gate = Arc::new(parking_lot::Mutex::new(()));
        mgr.set_compaction_gate(gate.clone());

        // Hold the gate before spawning the compaction thread.
        let held = gate.lock();

        let started = Arc::new(AtomicBool::new(false));
        let done = Arc::new(AtomicBool::new(false));
        let started2 = started.clone();
        let done2 = done.clone();

        std::thread::scope(|s| {
            s.spawn(|| {
                started2.store(true, Ordering::SeqCst);
                mgr.run_once_for_test();
                done2.store(true, Ordering::SeqCst);
            });

            // Give the spawned thread time to start and block on the gate.
            std::thread::sleep(Duration::from_millis(50));
            assert!(
                started.load(Ordering::SeqCst),
                "compaction thread never started"
            );
            assert!(
                !done.load(Ordering::SeqCst),
                "compaction ran to completion while the gate was held"
            );

            drop(held);
        });

        assert!(
            done.load(Ordering::SeqCst),
            "compaction did not complete after the gate was released"
        );
    }
}
