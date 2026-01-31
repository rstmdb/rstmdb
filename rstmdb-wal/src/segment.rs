//! WAL segment management.
//!
//! The WAL is split into fixed-size segments for easier management:
//! - Rotation: New segment when current exceeds size limit
//! - Cleanup: Old segments can be deleted after snapshotting
//! - Recovery: Segments can be read independently

use crate::entry::WalRecord;
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

/// A single WAL segment file.
pub struct Segment {
    id: SegmentId,
    path: PathBuf,
    file: File,
    size: u64,
    max_size: u64,
    sync_pending: bool,
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
        let mut records = Vec::new();
        let mut offset = 0u64;

        self.file.seek(SeekFrom::Start(0))?;
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
                        records.push((record_offset, record));
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
        self.file.seek(SeekFrom::End(0))?;
        self.sync()?;
        Ok(())
    }
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
}
