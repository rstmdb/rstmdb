//! WAL error types.

use thiserror::Error;

/// Errors that can occur during WAL operations.
#[derive(Debug, Error)]
pub enum WalError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("record corrupted at offset {offset}: CRC mismatch (expected {expected:#x}, got {actual:#x})")]
    CorruptedRecord {
        offset: u64,
        expected: u32,
        actual: u32,
    },

    #[error("invalid record header at offset {offset}: {reason}")]
    InvalidHeader { offset: u64, reason: String },

    #[error("record too large: {size} bytes (max {max})")]
    RecordTooLarge { size: usize, max: usize },

    #[error("segment not found: {0}")]
    SegmentNotFound(u64),

    #[error("WAL offset {requested} is before earliest available {earliest}")]
    OffsetTooOld { requested: u64, earliest: u64 },

    #[error("WAL is closed")]
    Closed,

    #[error("invalid WAL state: {0}")]
    InvalidState(String),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),
}

impl WalError {
    /// Returns whether this error is retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(self, WalError::Io(_))
    }
}
