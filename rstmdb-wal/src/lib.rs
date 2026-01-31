//! # rstmdb-wal
//!
//! Write-Ahead Log implementation for rstmdb.
//!
//! This crate provides a durable, append-only log with:
//! - Per-record checksums for corruption detection
//! - Segment-based file management
//! - Configurable fsync policies
//! - Recovery from partial writes

pub mod entry;
pub mod error;
pub mod recovery;
pub mod segment;
pub mod wal;

pub use entry::{WalEntry, WalEntryType, WalRecord};
pub use error::WalError;
pub use segment::{Segment, SegmentId};
pub use wal::{FsyncPolicy, Wal, WalConfig, WalOffset, WalReader, WalWriter};

/// Default segment size (64 MiB).
pub const DEFAULT_SEGMENT_SIZE: u64 = 64 * 1024 * 1024;

/// WAL record header size in bytes.
pub const RECORD_HEADER_SIZE: usize = 24;
