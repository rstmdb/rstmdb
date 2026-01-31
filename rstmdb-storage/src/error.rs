//! Storage error types.

use thiserror::Error;

/// Errors from the storage layer.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("snapshot not found: {0}")]
    SnapshotNotFound(String),

    #[error("instance not found: {0}")]
    InstanceNotFound(String),

    #[error("machine not found: {machine} v{version}")]
    MachineNotFound { machine: String, version: u32 },

    #[error("data corruption: {0}")]
    Corruption(String),

    #[error("WAL error: {0}")]
    Wal(#[from] rstmdb_wal::WalError),

    #[error("core error: {0}")]
    Core(#[from] rstmdb_core::CoreError),
}
