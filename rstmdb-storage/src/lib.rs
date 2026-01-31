//! # rstmdb-storage
//!
//! Storage layer for rstmdb.
//!
//! This crate provides:
//! - Snapshot storage and retrieval
//! - Instance persistence
//! - Machine definition storage
//! - Idempotency index

pub mod error;
pub mod index;
pub mod snapshot;
pub mod store;

pub use error::StorageError;
pub use index::IdempotencyIndex;
pub use snapshot::{SnapshotMeta, SnapshotPolicy, SnapshotStore};
pub use store::{CompactionResult, Storage};
