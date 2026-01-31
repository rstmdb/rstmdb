//! Main WAL implementation.

use crate::entry::{WalEntry, WalRecord};
use crate::error::WalError;
use crate::segment::{Segment, SegmentId, SegmentScanner};
use crate::DEFAULT_SEGMENT_SIZE;
use bytes::Bytes;
use parking_lot::{Mutex, RwLock};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// Fsync policy for WAL writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FsyncPolicy {
    /// Fsync after every write (safest, slowest).
    #[default]
    EveryWrite,
    /// Fsync after N writes.
    EveryN(u32),
    /// Fsync after N milliseconds (group commit).
    EveryMs(u32),
    /// Never fsync automatically (caller must call sync).
    Never,
}

/// WAL configuration.
#[derive(Debug, Clone)]
pub struct WalConfig {
    /// Directory to store WAL segments.
    pub dir: PathBuf,
    /// Maximum segment size before rotation.
    pub segment_size: u64,
    /// Fsync policy.
    pub fsync_policy: FsyncPolicy,
}

impl WalConfig {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            segment_size: DEFAULT_SEGMENT_SIZE,
            fsync_policy: FsyncPolicy::default(),
        }
    }

    pub fn with_segment_size(mut self, size: u64) -> Self {
        self.segment_size = size;
        self
    }

    pub fn with_fsync_policy(mut self, policy: FsyncPolicy) -> Self {
        self.fsync_policy = policy;
        self
    }
}

/// Global WAL offset: (segment_id, offset_within_segment)
/// We encode this as a single u64: segment_id << 40 | offset
/// This gives us ~1TB per segment and ~1 million segments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WalOffset(u64);

impl WalOffset {
    const OFFSET_BITS: u64 = 40;
    const OFFSET_MASK: u64 = (1 << Self::OFFSET_BITS) - 1;

    pub fn new(segment_id: SegmentId, offset: u64) -> Self {
        assert!(offset <= Self::OFFSET_MASK, "offset too large");
        Self((segment_id << Self::OFFSET_BITS) | offset)
    }

    pub fn segment_id(&self) -> SegmentId {
        self.0 >> Self::OFFSET_BITS
    }

    pub fn offset(&self) -> u64 {
        self.0 & Self::OFFSET_MASK
    }

    pub fn as_u64(&self) -> u64 {
        self.0
    }

    pub fn from_u64(value: u64) -> Self {
        Self(value)
    }
}

/// I/O statistics for the WAL.
#[derive(Debug, Clone, Copy, Default)]
pub struct WalStats {
    /// Total bytes written to WAL.
    pub bytes_written: u64,
    /// Total bytes read from WAL.
    pub bytes_read: u64,
    /// Total write operations.
    pub writes: u64,
    /// Total read operations.
    pub reads: u64,
    /// Total fsync operations.
    pub fsyncs: u64,
}

/// Write-Ahead Log.
pub struct Wal {
    config: WalConfig,
    /// Current segment for writing.
    current_segment: Mutex<Option<Segment>>,
    /// All open segments for reading.
    segments: RwLock<BTreeMap<SegmentId, Arc<Mutex<Segment>>>>,
    /// Next sequence number.
    next_sequence: AtomicU64,
    /// Writes since last fsync (for EveryN policy).
    writes_since_sync: AtomicU64,
    /// Is the WAL closed?
    closed: AtomicBool,
    /// I/O statistics counters.
    stats_bytes_written: AtomicU64,
    stats_bytes_read: AtomicU64,
    stats_writes: AtomicU64,
    stats_reads: AtomicU64,
    stats_fsyncs: AtomicU64,
}

impl Wal {
    /// Opens or creates a WAL at the configured directory.
    pub fn open(config: WalConfig) -> Result<Self, WalError> {
        // Create directory if it doesn't exist
        std::fs::create_dir_all(&config.dir)?;

        let wal = Self {
            config: config.clone(),
            current_segment: Mutex::new(None),
            segments: RwLock::new(BTreeMap::new()),
            next_sequence: AtomicU64::new(1),
            writes_since_sync: AtomicU64::new(0),
            closed: AtomicBool::new(false),
            stats_bytes_written: AtomicU64::new(0),
            stats_bytes_read: AtomicU64::new(0),
            stats_writes: AtomicU64::new(0),
            stats_reads: AtomicU64::new(0),
            stats_fsyncs: AtomicU64::new(0),
        };

        // Recover existing segments
        wal.recover()?;

        Ok(wal)
    }

    /// Recovers the WAL from existing segments.
    fn recover(&self) -> Result<(), WalError> {
        let segment_ids = SegmentScanner::list_segments(&self.config.dir)?;

        if segment_ids.is_empty() {
            // No existing segments, create first one
            self.rotate_segment()?;
            return Ok(());
        }

        let mut max_sequence = 0u64;

        // Open all segments and find max sequence
        for &seg_id in &segment_ids {
            let mut segment = Segment::open(&self.config.dir, seg_id, self.config.segment_size)?;

            // Read all records to find max sequence
            let records = segment.read_all()?;
            for (_, record) in &records {
                max_sequence = max_sequence.max(record.header.sequence);
            }

            self.segments
                .write()
                .insert(seg_id, Arc::new(Mutex::new(segment)));
        }

        // Set next sequence
        self.next_sequence.store(max_sequence + 1, Ordering::SeqCst);

        // Set current segment to latest
        let latest_id = *segment_ids.last().unwrap();
        let latest = self.segments.read().get(&latest_id).cloned();
        if let Some(seg) = latest {
            let segment = {
                let guard = seg.lock();
                Segment::open(&self.config.dir, guard.id(), self.config.segment_size)?
            };
            *self.current_segment.lock() = Some(segment);
        }

        tracing::info!(
            "WAL recovered: {} segments, next_sequence={}",
            segment_ids.len(),
            max_sequence + 1
        );

        Ok(())
    }

    /// Rotates to a new segment.
    fn rotate_segment(&self) -> Result<(), WalError> {
        let segments = self.segments.read();
        let next_id = segments.keys().next_back().map(|&id| id + 1).unwrap_or(1);
        drop(segments);

        let segment = Segment::create(&self.config.dir, next_id, self.config.segment_size)?;

        // Add to segments map
        self.segments.write().insert(
            next_id,
            Arc::new(Mutex::new(Segment::open(
                &self.config.dir,
                next_id,
                self.config.segment_size,
            )?)),
        );

        *self.current_segment.lock() = Some(segment);

        tracing::debug!("Rotated to segment {}", next_id);
        Ok(())
    }

    /// Appends an entry to the WAL.
    pub fn append(&self, entry: &WalEntry) -> Result<(u64, WalOffset), WalError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(WalError::Closed);
        }

        let payload = serde_json::to_vec(entry)?;
        let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
        let record = WalRecord::new(entry.entry_type(), sequence, Bytes::from(payload));
        let record_size = record.disk_size();

        let mut current = self.current_segment.lock();

        // Check if we need to rotate
        if current.is_none() || !current.as_ref().unwrap().can_fit(record_size) {
            drop(current);
            self.rotate_segment()?;
            current = self.current_segment.lock();
        }

        let segment = current.as_mut().unwrap();
        let segment_id = segment.id();
        let offset = segment.append(&record)?;

        // Update I/O statistics
        self.stats_bytes_written
            .fetch_add(record_size as u64, Ordering::Relaxed);
        self.stats_writes.fetch_add(1, Ordering::Relaxed);

        // Handle fsync policy
        let writes = self.writes_since_sync.fetch_add(1, Ordering::Relaxed) + 1;
        match self.config.fsync_policy {
            FsyncPolicy::EveryWrite => {
                segment.sync()?;
                self.stats_fsyncs.fetch_add(1, Ordering::Relaxed);
                self.writes_since_sync.store(0, Ordering::Relaxed);
            }
            FsyncPolicy::EveryN(n) if writes >= n as u64 => {
                segment.sync()?;
                self.stats_fsyncs.fetch_add(1, Ordering::Relaxed);
                self.writes_since_sync.store(0, Ordering::Relaxed);
            }
            _ => {}
        }

        Ok((sequence, WalOffset::new(segment_id, offset)))
    }

    /// Forces a sync to disk.
    pub fn sync(&self) -> Result<(), WalError> {
        let mut current = self.current_segment.lock();
        if let Some(segment) = current.as_mut() {
            segment.sync()?;
            self.stats_fsyncs.fetch_add(1, Ordering::Relaxed);
        }
        self.writes_since_sync.store(0, Ordering::Relaxed);
        Ok(())
    }

    /// Returns the current I/O statistics.
    pub fn stats(&self) -> WalStats {
        WalStats {
            bytes_written: self.stats_bytes_written.load(Ordering::Relaxed),
            bytes_read: self.stats_bytes_read.load(Ordering::Relaxed),
            writes: self.stats_writes.load(Ordering::Relaxed),
            reads: self.stats_reads.load(Ordering::Relaxed),
            fsyncs: self.stats_fsyncs.load(Ordering::Relaxed),
        }
    }

    /// Returns the next sequence number that will be assigned.
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence.load(Ordering::SeqCst)
    }

    /// Reads entries from the given offset.
    pub fn read_from(
        &self,
        from: WalOffset,
        limit: Option<usize>,
    ) -> Result<Vec<(u64, WalOffset, WalEntry)>, WalError> {
        let segments = self.segments.read();
        let mut results = Vec::new();
        let mut remaining = limit.unwrap_or(usize::MAX);
        let mut bytes_read = 0u64;

        for (&seg_id, segment) in segments.range(from.segment_id()..) {
            if remaining == 0 {
                break;
            }

            let mut seg = segment.lock();
            let records = seg.read_all()?;

            for (offset, record) in records {
                let wal_offset = WalOffset::new(seg_id, offset);
                if wal_offset < from {
                    continue;
                }

                bytes_read += record.disk_size() as u64;
                let entry: WalEntry = serde_json::from_slice(&record.payload)?;
                results.push((record.header.sequence, wal_offset, entry));

                remaining -= 1;
                if remaining == 0 {
                    break;
                }
            }
        }

        // Update I/O statistics
        self.stats_bytes_read
            .fetch_add(bytes_read, Ordering::Relaxed);
        self.stats_reads.fetch_add(1, Ordering::Relaxed);

        Ok(results)
    }

    /// Closes the WAL.
    pub fn close(&self) -> Result<(), WalError> {
        self.closed.store(true, Ordering::Release);
        self.sync()?;
        Ok(())
    }

    /// Returns the earliest available offset.
    pub fn earliest_offset(&self) -> Option<WalOffset> {
        let segments = self.segments.read();
        segments.keys().next().map(|&id| WalOffset::new(id, 0))
    }

    /// Returns the latest offset.
    pub fn latest_offset(&self) -> Option<WalOffset> {
        let current = self.current_segment.lock();
        current
            .as_ref()
            .map(|seg| WalOffset::new(seg.id(), seg.size()))
    }

    /// Returns the list of segment IDs.
    pub fn segment_ids(&self) -> Vec<SegmentId> {
        self.segments.read().keys().copied().collect()
    }

    /// Compacts the WAL by deleting segments older than the given offset.
    ///
    /// This should only be called after ensuring all instances have snapshots
    /// at or after the given offset.
    ///
    /// Returns the number of segments deleted.
    pub fn compact_before(&self, before_offset: WalOffset) -> Result<usize, WalError> {
        let target_segment_id = before_offset.segment_id();

        // Find segments to delete (all segments before the target)
        let segments_to_delete: Vec<SegmentId> = {
            let segments = self.segments.read();
            segments
                .keys()
                .filter(|&&id| id < target_segment_id)
                .copied()
                .collect()
        };

        if segments_to_delete.is_empty() {
            return Ok(0);
        }

        let mut deleted = 0;

        for seg_id in &segments_to_delete {
            // Remove from segments map
            let segment = self.segments.write().remove(seg_id);

            if let Some(seg) = segment {
                // Get path and drop the lock
                let path = {
                    let guard = seg.lock();
                    guard.path().to_path_buf()
                };

                // Delete the file
                if path.exists() {
                    std::fs::remove_file(&path)?;
                    tracing::info!("Compacted WAL segment {}", seg_id);
                    deleted += 1;
                }
            }
        }

        Ok(deleted)
    }

    /// Returns the total size of all segments in bytes.
    pub fn total_size(&self) -> u64 {
        let segments = self.segments.read();
        let mut total: u64 = segments.values().map(|s| s.lock().size()).sum();

        // Also include current segment if it's not in the map
        let current = self.current_segment.lock();
        if let Some(seg) = current.as_ref() {
            let current_id = seg.id();
            if !segments.contains_key(&current_id) {
                total += seg.size();
            }
        }

        total
    }
}

/// WAL writer handle for appending entries.
pub struct WalWriter {
    wal: Arc<Wal>,
}

impl WalWriter {
    pub fn new(wal: Arc<Wal>) -> Self {
        Self { wal }
    }

    pub fn append(&self, entry: &WalEntry) -> Result<(u64, WalOffset), WalError> {
        self.wal.append(entry)
    }

    pub fn sync(&self) -> Result<(), WalError> {
        self.wal.sync()
    }
}

/// WAL reader handle for reading entries.
pub struct WalReader {
    wal: Arc<Wal>,
}

impl WalReader {
    pub fn new(wal: Arc<Wal>) -> Self {
        Self { wal }
    }

    pub fn read_from(
        &self,
        from: WalOffset,
        limit: Option<usize>,
    ) -> Result<Vec<(u64, WalOffset, WalEntry)>, WalError> {
        self.wal.read_from(from, limit)
    }

    pub fn earliest_offset(&self) -> Option<WalOffset> {
        self.wal.earliest_offset()
    }

    pub fn latest_offset(&self) -> Option<WalOffset> {
        self.wal.latest_offset()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use tempfile::TempDir;

    fn test_config(dir: &Path) -> WalConfig {
        WalConfig::new(dir)
            .with_segment_size(4096) // Small segments for testing
            .with_fsync_policy(FsyncPolicy::EveryWrite)
    }

    #[test]
    fn test_wal_append_and_read() {
        let dir = TempDir::new().unwrap();
        let wal = Wal::open(test_config(dir.path())).unwrap();

        let entry = WalEntry::CreateInstance {
            instance_id: "i-1".to_string(),
            machine: "order".to_string(),
            version: 1,
            initial_state: "created".to_string(),
            initial_ctx: serde_json::json!({}),
            idempotency_key: None,
        };

        let (seq, _offset) = wal.append(&entry).unwrap();
        assert_eq!(seq, 1);

        let entries = wal.read_from(WalOffset::new(1, 0), None).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, 1);
    }

    #[test]
    fn test_wal_recovery() {
        let dir = TempDir::new().unwrap();

        // Write some entries
        {
            let wal = Wal::open(test_config(dir.path())).unwrap();
            for i in 0..10 {
                let entry = WalEntry::ApplyEvent {
                    instance_id: "i-1".to_string(),
                    event: format!("E{}", i),
                    from_state: "s1".to_string(),
                    to_state: "s2".to_string(),
                    payload: serde_json::json!({}),
                    ctx: serde_json::json!({}),
                    event_id: None,
                    idempotency_key: None,
                };
                wal.append(&entry).unwrap();
            }
            wal.close().unwrap();
        }

        // Reopen and verify
        {
            let wal = Wal::open(test_config(dir.path())).unwrap();
            assert_eq!(wal.next_sequence(), 11);

            let entries = wal.read_from(WalOffset::new(1, 0), None).unwrap();
            assert_eq!(entries.len(), 10);
        }
    }

    #[test]
    fn test_wal_segment_rotation() {
        let dir = TempDir::new().unwrap();
        let config = WalConfig::new(dir.path())
            .with_segment_size(512) // Very small to force rotation
            .with_fsync_policy(FsyncPolicy::EveryWrite);

        let wal = Wal::open(config).unwrap();

        // Write enough data to force segment rotation
        for i in 0..20 {
            let entry = WalEntry::ApplyEvent {
                instance_id: "i-1".to_string(),
                event: format!("EVENT_{}", i),
                from_state: "s1".to_string(),
                to_state: "s2".to_string(),
                payload: serde_json::json!({"data": "some payload data here"}),
                ctx: serde_json::json!({}),
                event_id: None,
                idempotency_key: None,
            };
            wal.append(&entry).unwrap();
        }

        // Verify multiple segments exist
        let segments = SegmentScanner::list_segments(dir.path()).unwrap();
        assert!(
            segments.len() > 1,
            "Expected multiple segments, got {}",
            segments.len()
        );

        // Verify all entries can still be read
        let entries = wal.read_from(WalOffset::new(1, 0), None).unwrap();
        assert_eq!(entries.len(), 20);
    }

    #[test]
    fn test_wal_compaction() {
        let dir = TempDir::new().unwrap();
        let config = WalConfig::new(dir.path())
            .with_segment_size(256) // Very small to force multiple segments
            .with_fsync_policy(FsyncPolicy::EveryWrite);

        let wal = Wal::open(config).unwrap();

        // Write enough data to create multiple segments
        for i in 0..30 {
            let entry = WalEntry::ApplyEvent {
                instance_id: "i-1".to_string(),
                event: format!("E{}", i),
                from_state: "s1".to_string(),
                to_state: "s2".to_string(),
                payload: serde_json::json!({"data": "test payload"}),
                ctx: serde_json::json!({}),
                event_id: None,
                idempotency_key: None,
            };
            wal.append(&entry).unwrap();
        }

        let segment_ids = wal.segment_ids();
        assert!(segment_ids.len() > 2, "Expected multiple segments");

        // Compact segments before the third segment
        if segment_ids.len() >= 3 {
            let compact_before = WalOffset::new(segment_ids[2], 0);
            let deleted = wal.compact_before(compact_before).unwrap();
            assert!(deleted >= 2, "Expected to delete at least 2 segments");

            // Verify remaining segments
            let remaining = wal.segment_ids();
            assert!(remaining.len() < segment_ids.len());
        }
    }

    #[test]
    fn test_wal_segment_ids() {
        let dir = TempDir::new().unwrap();
        let wal = Wal::open(test_config(dir.path())).unwrap();

        let ids = wal.segment_ids();
        assert!(!ids.is_empty(), "Should have at least one segment");
    }

    #[test]
    fn test_compact_empty_wal() {
        let dir = TempDir::new().unwrap();
        let wal = Wal::open(test_config(dir.path())).unwrap();

        // Compacting with offset 0 should not delete the only segment
        let deleted = wal.compact_before(WalOffset::new(0, 0)).unwrap();
        assert_eq!(deleted, 0);
    }
}
