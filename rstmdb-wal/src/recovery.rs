//! WAL recovery utilities.
//!
//! Handles recovery from:
//! - Partial writes (incomplete records at end of segment)
//! - Corrupted records
//! - Missing segments

use crate::entry::WalRecord;
use crate::error::WalError;
use crate::segment::{Segment, SegmentId, SegmentScanner};
use crate::RECORD_HEADER_SIZE;
use bytes::BytesMut;
use std::path::Path;

/// Result of WAL recovery scan.
#[derive(Debug)]
pub struct RecoveryResult {
    /// Number of valid records found.
    pub valid_records: u64,
    /// Number of corrupted/partial records found.
    pub invalid_records: u64,
    /// Bytes truncated due to partial writes.
    pub bytes_truncated: u64,
    /// Segments that were recovered.
    pub segments_recovered: Vec<SegmentId>,
    /// Segments that had errors.
    pub segments_with_errors: Vec<(SegmentId, String)>,
    /// Maximum sequence number found.
    pub max_sequence: u64,
}

/// WAL recovery scanner.
pub struct RecoveryScanner {
    dir: std::path::PathBuf,
    segment_size: u64,
}

impl RecoveryScanner {
    pub fn new(dir: impl AsRef<Path>, segment_size: u64) -> Self {
        Self {
            dir: dir.as_ref().to_path_buf(),
            segment_size,
        }
    }

    /// Scans and optionally repairs the WAL.
    pub fn scan(&self, repair: bool) -> Result<RecoveryResult, WalError> {
        let segment_ids = SegmentScanner::list_segments(&self.dir)?;

        let mut result = RecoveryResult {
            valid_records: 0,
            invalid_records: 0,
            bytes_truncated: 0,
            segments_recovered: Vec::new(),
            segments_with_errors: Vec::new(),
            max_sequence: 0,
        };

        for seg_id in segment_ids {
            match self.scan_segment(seg_id, repair) {
                Ok((valid, invalid, truncated, max_seq)) => {
                    result.valid_records += valid;
                    result.invalid_records += invalid;
                    result.bytes_truncated += truncated;
                    result.max_sequence = result.max_sequence.max(max_seq);
                    if invalid > 0 || truncated > 0 {
                        result.segments_recovered.push(seg_id);
                    }
                }
                Err(e) => {
                    result.segments_with_errors.push((seg_id, e.to_string()));
                }
            }
        }

        Ok(result)
    }

    /// Scans a single segment.
    fn scan_segment(
        &self,
        seg_id: SegmentId,
        repair: bool,
    ) -> Result<(u64, u64, u64, u64), WalError> {
        let mut segment = Segment::open(&self.dir, seg_id, self.segment_size)?;
        let file_size = segment.size();

        let mut valid_records = 0u64;
        let mut invalid_records = 0u64;
        let mut max_sequence = 0u64;
        let mut last_valid_offset = 0u64;
        let mut offset = 0u64;

        // Read file into buffer
        let mut buf = BytesMut::with_capacity(file_size as usize);
        let mut file_buf = vec![0u8; file_size as usize];

        // Use read_at equivalent by seeking
        {
            use std::io::Read;
            let mut file = std::fs::File::open(segment.path())?;
            file.read_exact(&mut file_buf)?;
        }
        buf.extend_from_slice(&file_buf);

        // Scan records
        while buf.len() >= RECORD_HEADER_SIZE {
            let record_offset = offset;
            match WalRecord::decode(&mut buf, record_offset) {
                Ok(Some(record)) => {
                    valid_records += 1;
                    max_sequence = max_sequence.max(record.header.sequence);
                    offset += record.disk_size() as u64;
                    last_valid_offset = offset;
                }
                Ok(None) => {
                    // Incomplete record at end - this is a partial write
                    break;
                }
                Err(WalError::CorruptedRecord { .. }) => {
                    invalid_records += 1;
                    // Skip to next potential record (scan for magic bytes)
                    if !buf.is_empty() {
                        buf.advance(1);
                        offset += 1;
                    }
                }
                Err(WalError::InvalidHeader { .. }) => {
                    // Could be padding or corruption
                    if buf.iter().all(|&b| b == 0) {
                        // All zeros, probably padding
                        break;
                    }
                    invalid_records += 1;
                    if !buf.is_empty() {
                        buf.advance(1);
                        offset += 1;
                    }
                }
                Err(e) => return Err(e),
            }
        }

        let bytes_truncated = file_size - last_valid_offset;

        // Repair if requested and needed
        if repair && bytes_truncated > 0 {
            segment.truncate_at(last_valid_offset)?;
            tracing::warn!(
                "Truncated segment {} at offset {} (removed {} bytes)",
                seg_id,
                last_valid_offset,
                bytes_truncated
            );
        }

        Ok((
            valid_records,
            invalid_records,
            bytes_truncated,
            max_sequence,
        ))
    }
}

use bytes::Buf;

/// Verifies WAL integrity without modifying anything.
pub fn verify_wal(dir: impl AsRef<Path>, segment_size: u64) -> Result<RecoveryResult, WalError> {
    RecoveryScanner::new(dir, segment_size).scan(false)
}

/// Repairs WAL by truncating partial writes.
pub fn repair_wal(dir: impl AsRef<Path>, segment_size: u64) -> Result<RecoveryResult, WalError> {
    RecoveryScanner::new(dir, segment_size).scan(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entry::WalEntryType;
    use bytes::Bytes;
    use tempfile::TempDir;

    #[test]
    fn test_recovery_clean_wal() {
        let dir = TempDir::new().unwrap();

        // Create a clean segment with valid records
        {
            let mut segment = Segment::create(dir.path(), 1, 4096).unwrap();
            for i in 0..5 {
                let record = WalRecord::new(
                    WalEntryType::ApplyEvent,
                    i + 1,
                    Bytes::from(format!(r#"{{"seq":{}}}"#, i)),
                );
                segment.append(&record).unwrap();
            }
            segment.sync().unwrap();
        }

        let result = verify_wal(dir.path(), 4096).unwrap();
        assert_eq!(result.valid_records, 5);
        assert_eq!(result.invalid_records, 0);
        assert_eq!(result.bytes_truncated, 0);
        assert_eq!(result.max_sequence, 5);
    }

    #[test]
    fn test_recovery_partial_write() {
        let dir = TempDir::new().unwrap();

        // Create a segment with a partial write at the end
        {
            let mut segment = Segment::create(dir.path(), 1, 4096).unwrap();
            for i in 0..3 {
                let record = WalRecord::new(
                    WalEntryType::ApplyEvent,
                    i + 1,
                    Bytes::from(format!(r#"{{"seq":{}}}"#, i)),
                );
                segment.append(&record).unwrap();
            }
            segment.sync().unwrap();

            // Append garbage to simulate partial write
            use std::io::Write;
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .open(segment.path())
                .unwrap();
            file.write_all(b"WLOG\x03\x00\x00\x00").unwrap(); // Partial header
        }

        // Verify detects the issue
        let result = verify_wal(dir.path(), 4096).unwrap();
        assert_eq!(result.valid_records, 3);
        assert!(result.bytes_truncated > 0);

        // Repair fixes it
        let result = repair_wal(dir.path(), 4096).unwrap();
        assert_eq!(result.valid_records, 3);

        // Verify again - should be clean
        let result = verify_wal(dir.path(), 4096).unwrap();
        assert_eq!(result.bytes_truncated, 0);
    }
}
