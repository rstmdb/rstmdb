//! WAL segment management.
//!
//! The WAL is split into fixed-size segments for easier management:
//! - Rotation: New segment when current exceeds size limit
//! - Cleanup: Old segments can be deleted after snapshotting
//! - Recovery: Segments can be read independently

use crate::entry::{WalRecord, WAL_MAGIC};
use crate::error::WalError;
use crate::RECORD_HEADER_SIZE;
use bytes::BytesMut;
use std::fs::{File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

/// Segment identifier (monotonically increasing).
pub type SegmentId = u64;

/// Segment file name format: NNNNNNNNNNNNNNNN.wal (16 hex digits)
pub fn segment_filename(id: SegmentId) -> String {
    format!("{:016x}.wal", id)
}

/// Parse segment ID from filename.
pub fn parse_segment_filename(name: &str) -> Option<SegmentId> {
    let name = name.strip_suffix(".wal")?;
    if name.len() != 16 {
        return None;
    }
    u64::from_str_radix(name, 16).ok()
}

/// Minimum spacing between in-memory record-boundary checkpoints.
const INDEX_INTERVAL: u64 = 64 * 1024;

/// A single WAL segment file.
pub struct Segment {
    id: SegmentId,
    path: PathBuf,
    file: File,
    size: u64,
    max_size: u64,
    sync_pending: bool,
    /// Sparse, sorted record start offsets (roughly one per `INDEX_INTERVAL`
    /// bytes), learned from appends and scans. Lets `read_from` resume near the
    /// requested offset instead of rescanning the segment from the start —
    /// the replication tailer reads the tail every few milliseconds.
    index: Vec<u64>,
}

impl Segment {
    /// Creates a new segment file.
    pub fn create(dir: &Path, id: SegmentId, max_size: u64) -> Result<Self, WalError> {
        let path = dir.join(segment_filename(id));
        let file = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&path)?;

        Ok(Self {
            id,
            path,
            file,
            size: 0,
            max_size,
            sync_pending: false,
            index: Vec::new(),
        })
    }

    /// Opens an existing segment file for reading and appending.
    pub fn open(dir: &Path, id: SegmentId, max_size: u64) -> Result<Self, WalError> {
        let path = dir.join(segment_filename(id));
        let file = OpenOptions::new().read(true).write(true).open(&path)?;

        let size = file.metadata()?.len();

        Ok(Self {
            id,
            path,
            file,
            size,
            max_size,
            sync_pending: false,
            index: Vec::new(),
        })
    }

    /// Returns the segment ID.
    pub fn id(&self) -> SegmentId {
        self.id
    }

    /// Returns the segment file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the current size of the segment.
    pub fn size(&self) -> u64 {
        self.size
    }

    /// Returns whether the segment is full.
    pub fn is_full(&self) -> bool {
        self.size >= self.max_size
    }

    /// Returns whether the segment can fit a record of the given size.
    pub fn can_fit(&self, record_size: usize) -> bool {
        self.size + record_size as u64 <= self.max_size
    }

    /// Appends a record to the segment.
    pub fn append(&mut self, record: &WalRecord) -> Result<u64, WalError> {
        let encoded = record.encode()?;
        let offset = self.size;

        self.file.seek(SeekFrom::End(0))?;
        self.file.write_all(&encoded)?;
        self.size += encoded.len() as u64;
        self.sync_pending = true;
        note_boundary(&mut self.index, offset);

        Ok(offset)
    }

    /// Syncs the segment to disk.
    pub fn sync(&mut self) -> Result<(), WalError> {
        if self.sync_pending {
            self.file.sync_data()?;
            self.sync_pending = false;
        }
        Ok(())
    }

    /// Reads all records from the segment.
    pub fn read_all(&mut self) -> Result<Vec<(u64, WalRecord)>, WalError> {
        self.read_from(0, None)
    }

    /// Reads records whose offset is `>= start`, at most `limit` of them.
    /// `start` need not be a record boundary: scanning resumes from the nearest
    /// known boundary at or before it, and records before `start` are skipped.
    ///
    /// Records before that boundary are not decoded, so corruption there does
    /// not surface here; `read_all` (and recovery) still scan from offset 0.
    pub fn read_from(
        &mut self,
        start: u64,
        limit: Option<usize>,
    ) -> Result<Vec<(u64, WalRecord)>, WalError> {
        let limit = limit.unwrap_or(usize::MAX);
        if limit == 0 {
            return Ok(Vec::new());
        }

        let checkpoint = self.checkpoint_for(start)?;
        match self.scan(checkpoint, start, limit) {
            // The file changed behind the index's back in a way the magic check
            // couldn't catch. Forget everything learned and rescan from 0.
            Err(_) if checkpoint > 0 => {
                self.index.clear();
                self.scan(0, start, limit)
            }
            result => result,
        }
    }

    /// Returns the nearest indexed record boundary at or before `start`,
    /// verifying it still lies within the file and begins with `WAL_MAGIC`.
    /// The index assumes the file only changes through this `Segment`; if it
    /// was rewritten or truncated out of band, the index is dropped and the
    /// scan restarts from 0.
    fn checkpoint_for(&mut self, start: u64) -> Result<u64, WalError> {
        let checkpoint = match self.index.partition_point(|&c| c <= start) {
            0 => return Ok(0),
            i => self.index[i - 1],
        };

        let mut magic = [0u8; 4];
        let valid = checkpoint + RECORD_HEADER_SIZE as u64 <= self.file.metadata()?.len() && {
            self.file.seek(SeekFrom::Start(checkpoint))?;
            self.file.read_exact(&mut magic)?;
            magic == WAL_MAGIC
        };
        if valid {
            Ok(checkpoint)
        } else {
            self.index.clear();
            Ok(0)
        }
    }

    /// Decodes records from `offset` (a record boundary) to EOF, returning up
    /// to `limit` of those at or after `start`.
    fn scan(
        &mut self,
        mut offset: u64,
        start: u64,
        limit: usize,
    ) -> Result<Vec<(u64, WalRecord)>, WalError> {
        let mut records = Vec::new();

        self.file.seek(SeekFrom::Start(offset))?;
        let mut reader = BufReader::new(&self.file);
        let mut buf = BytesMut::new();

        loop {
            // Read more data if needed
            let mut chunk = vec![0u8; 8192];
            match reader.read(&mut chunk) {
                Ok(0) => break, // EOF
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(e.into()),
            }

            // Try to decode records
            while buf.len() >= RECORD_HEADER_SIZE {
                let record_offset = offset;
                match WalRecord::decode(&mut buf, record_offset)? {
                    Some(record) => {
                        let record_size = record.disk_size();
                        note_boundary(&mut self.index, record_offset);
                        if record_offset >= start {
                            records.push((record_offset, record));
                            if records.len() == limit {
                                return Ok(records);
                            }
                        }
                        offset += record_size as u64;
                    }
                    None => break, // Need more data
                }
            }
        }

        Ok(records)
    }

    /// Reads a single record at the given offset.
    pub fn read_at(&mut self, offset: u64) -> Result<Option<WalRecord>, WalError> {
        self.file.seek(SeekFrom::Start(offset))?;

        let mut buf = BytesMut::with_capacity(RECORD_HEADER_SIZE + 4096);
        let mut chunk = vec![0u8; RECORD_HEADER_SIZE + 4096];

        loop {
            match self.file.read(&mut chunk) {
                Ok(0) => return Ok(None), // EOF
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(e.into()),
            }

            match WalRecord::decode(&mut buf, offset)? {
                Some(record) => return Ok(Some(record)),
                None => continue, // Need more data
            }
        }
    }

    /// Truncates the segment at the given offset (for recovery from partial writes).
    pub fn truncate_at(&mut self, offset: u64) -> Result<(), WalError> {
        self.file.set_len(offset)?;
        self.size = offset;
        self.index.retain(|&c| c < offset);
        self.file.seek(SeekFrom::End(0))?;
        self.sync()?;
        Ok(())
    }
}

/// Records `offset` (a record boundary) in the sparse index unless a
/// checkpoint already exists within `INDEX_INTERVAL` bytes before it.
fn note_boundary(index: &mut Vec<u64>, offset: u64) {
    let i = index.partition_point(|&c| c <= offset);
    if i > 0 && offset < index[i - 1] + INDEX_INTERVAL {
        return;
    }
    index.insert(i, offset);
}

/// Segment directory scanner.
pub struct SegmentScanner;

impl SegmentScanner {
    /// Lists all segment IDs in a directory, sorted ascending.
    pub fn list_segments(dir: &Path) -> Result<Vec<SegmentId>, WalError> {
        let mut segments = Vec::new();

        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(id) = parse_segment_filename(&name) {
                segments.push(id);
            }
        }

        segments.sort();
        Ok(segments)
    }

    /// Returns the latest segment ID, or None if no segments exist.
    pub fn latest_segment(dir: &Path) -> Result<Option<SegmentId>, WalError> {
        let segments = Self::list_segments(dir)?;
        Ok(segments.last().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::WalEntryType;
    use crate::DEFAULT_SEGMENT_SIZE;
    use bytes::Bytes;
    use tempfile::TempDir;

    #[test]
    fn test_segment_filename() {
        assert_eq!(segment_filename(0), "0000000000000000.wal");
        assert_eq!(segment_filename(255), "00000000000000ff.wal");
        assert_eq!(segment_filename(0xDEADBEEF), "00000000deadbeef.wal");
    }

    #[test]
    fn test_parse_segment_filename() {
        assert_eq!(parse_segment_filename("0000000000000000.wal"), Some(0));
        assert_eq!(parse_segment_filename("00000000000000ff.wal"), Some(255));
        assert_eq!(parse_segment_filename("invalid.wal"), None);
        assert_eq!(parse_segment_filename("0000000000000000.txt"), None);
    }

    #[test]
    fn test_segment_create_and_append() {
        let dir = TempDir::new().unwrap();
        let mut segment = Segment::create(dir.path(), 1, DEFAULT_SEGMENT_SIZE).unwrap();

        let record = WalRecord::new(
            WalEntryType::ApplyEvent,
            1,
            Bytes::from(r#"{"test":"data"}"#),
        );
        let offset = segment.append(&record).unwrap();
        assert_eq!(offset, 0);

        segment.sync().unwrap();
        assert!(segment.size() > 0);
    }

    #[test]
    fn test_segment_read_all() {
        let dir = TempDir::new().unwrap();
        let mut segment = Segment::create(dir.path(), 1, DEFAULT_SEGMENT_SIZE).unwrap();

        // Write multiple records
        for i in 0..5 {
            let record = WalRecord::new(
                WalEntryType::ApplyEvent,
                i,
                Bytes::from(format!(r#"{{"seq":{}}}"#, i)),
            );
            segment.append(&record).unwrap();
        }
        segment.sync().unwrap();

        // Read them back
        let records = segment.read_all().unwrap();
        assert_eq!(records.len(), 5);
        for (i, (_, record)) in records.iter().enumerate() {
            assert_eq!(record.header.sequence, i as u64);
        }
    }

    /// Appends `n` records with ~1KB payloads so the segment spans many index
    /// checkpoints. Returns the record start offsets.
    fn fill_segment(segment: &mut Segment, n: u64) -> Vec<u64> {
        (0..n)
            .map(|i| {
                let payload = format!(r#"{{"seq":{},"pad":"{}"}}"#, i, "x".repeat(1000));
                let record = WalRecord::new(WalEntryType::ApplyEvent, i, Bytes::from(payload));
                segment.append(&record).unwrap()
            })
            .collect()
    }

    /// `read_from(start)` must return exactly the records whose offset is
    /// `>= start`, for record boundaries and arbitrary mid-record offsets alike.
    fn assert_read_from_matches(segment: &mut Segment, offsets: &[u64]) {
        let probes = offsets
            .iter()
            .flat_map(|&o| [o, o + 1, o.saturating_sub(1)])
            .chain([0, segment.size(), segment.size() + 10]);
        for start in probes {
            let expected: Vec<u64> = offsets.iter().copied().filter(|&o| o >= start).collect();
            let got: Vec<u64> = segment
                .read_from(start, None)
                .unwrap()
                .into_iter()
                .map(|(o, _)| o)
                .collect();
            assert_eq!(got, expected, "read_from({})", start);
        }
    }

    #[test]
    fn test_segment_read_from_fresh_segment() {
        let dir = TempDir::new().unwrap();
        let mut segment = Segment::create(dir.path(), 1, DEFAULT_SEGMENT_SIZE).unwrap();
        let offsets = fill_segment(&mut segment, 300);
        assert_read_from_matches(&mut segment, &offsets);
    }

    #[test]
    fn test_segment_read_from_reopened_segment() {
        let dir = TempDir::new().unwrap();
        let offsets = {
            let mut segment = Segment::create(dir.path(), 1, DEFAULT_SEGMENT_SIZE).unwrap();
            let offsets = fill_segment(&mut segment, 300);
            segment.sync().unwrap();
            offsets
        };

        // Reopened: no in-memory index yet. A read from the tail must still be
        // correct (before and after the first full scan builds the index).
        let mut segment = Segment::open(dir.path(), 1, DEFAULT_SEGMENT_SIZE).unwrap();
        let last = *offsets.last().unwrap();
        assert_eq!(segment.read_from(last, None).unwrap().len(), 1);
        assert_read_from_matches(&mut segment, &offsets);

        // Appends after reopen keep the index consistent.
        let mut all = offsets.clone();
        all.extend(fill_segment(&mut segment, 50));
        assert_read_from_matches(&mut segment, &all);
    }

    #[test]
    fn test_segment_read_from_after_truncate() {
        let dir = TempDir::new().unwrap();
        let mut segment = Segment::create(dir.path(), 1, DEFAULT_SEGMENT_SIZE).unwrap();
        let offsets = fill_segment(&mut segment, 300);

        // Truncate mid-segment, then append different records over the old tail:
        // stale checkpoints past the cut must not be used.
        let cut = offsets[120];
        segment.truncate_at(cut).unwrap();
        let mut all = offsets[..120].to_vec();
        for i in 0..200u64 {
            let payload = format!(r#"{{"new":{},"pad":"{}"}}"#, i, "y".repeat(333));
            let record = WalRecord::new(WalEntryType::ApplyEvent, 1000 + i, Bytes::from(payload));
            all.push(segment.append(&record).unwrap());
        }
        assert_read_from_matches(&mut segment, &all);
    }

    #[test]
    fn test_segment_read_from_survives_out_of_band_rewrite() {
        let dir = TempDir::new().unwrap();
        let mut segment = Segment::create(dir.path(), 1, DEFAULT_SEGMENT_SIZE).unwrap();
        let old = fill_segment(&mut segment, 300);
        segment.read_all().unwrap(); // warm the index

        // Rewrite the file in place behind this handle's back (manual repair,
        // copy over a live file) with differently sized records, so learned
        // checkpoints land mid-record or past the new EOF.
        let other = TempDir::new().unwrap();
        let mut replacement = Segment::create(other.path(), 1, DEFAULT_SEGMENT_SIZE).unwrap();
        let mut new = Vec::new();
        for i in 0..150u64 {
            let payload = format!(r#"{{"r":{},"pad":"{}"}}"#, i, "z".repeat(777));
            let record = WalRecord::new(WalEntryType::ApplyEvent, i, Bytes::from(payload));
            new.push(replacement.append(&record).unwrap());
        }
        replacement.sync().unwrap();
        std::fs::copy(replacement.path(), segment.path()).unwrap();
        assert!(old.last() > new.last());

        assert_read_from_matches(&mut segment, &new);
    }

    #[test]
    fn test_segment_read_from_limit() {
        let dir = TempDir::new().unwrap();
        let mut segment = Segment::create(dir.path(), 1, DEFAULT_SEGMENT_SIZE).unwrap();
        let offsets = fill_segment(&mut segment, 300);

        for (start, limit) in [(0, 0), (0, 5), (offsets[100] + 1, 7), (offsets[290], 50)] {
            let expected: Vec<u64> = offsets
                .iter()
                .copied()
                .filter(|&o| o >= start)
                .take(limit)
                .collect();
            let got: Vec<u64> = segment
                .read_from(start, Some(limit))
                .unwrap()
                .into_iter()
                .map(|(o, _)| o)
                .collect();
            assert_eq!(got, expected, "read_from({}, Some({}))", start, limit);
        }
    }
}
