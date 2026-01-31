//! Automatic compaction manager.

use crate::config::CompactionConfig;
use rstmdb_core::instance::InstanceSnapshot;
use rstmdb_core::StateMachineEngine;
use rstmdb_storage::SnapshotStore;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

/// Manages automatic WAL compaction.
pub struct CompactionManager {
    engine: Arc<StateMachineEngine>,
    snapshot_store: Arc<SnapshotStore>,
    config: CompactionConfig,
    events_since_compact: AtomicU64,
    last_compact: parking_lot::Mutex<Instant>,
    shutdown: AtomicBool,
    notify: Notify,
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
        }
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
        let mut result = CompactionResult::default();

        // Snapshot instances that have changed
        for instance in self.engine.get_all_instances() {
            let needs_snapshot = match self.snapshot_store.get_snapshot_meta(&instance.id) {
                Some(meta) => instance.last_wal_offset > meta.wal_offset,
                None => true,
            };

            if needs_snapshot {
                let snapshot_id = format!("snap-{}", uuid::Uuid::new_v4());
                let snapshot = InstanceSnapshot::from_instance(&instance, snapshot_id);
                if self.snapshot_store.create_snapshot(&snapshot).is_ok() {
                    result.snapshots_created += 1;
                }
            }
        }

        // Compact WAL
        if let Some(min_offset) = self.snapshot_store.min_wal_offset() {
            if let Ok(deleted) = self
                .engine
                .wal()
                .compact_before(rstmdb_wal::WalOffset::from_u64(min_offset))
            {
                result.segments_deleted = deleted;
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
