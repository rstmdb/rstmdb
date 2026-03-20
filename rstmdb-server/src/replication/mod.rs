//! WAL streaming replication.
//!
//! This module implements primary-replica replication via WAL entry streaming.
//! The primary streams WAL entries to connected replicas. In async mode, writes
//! return immediately; in sync mode, the primary waits for ACKs from replicas.

pub mod lag_monitor;
pub mod manager;
pub mod protocol;
pub mod replica_client;

pub use manager::ReplicationManager;
pub use protocol::ReplicationMessage;
pub use replica_client::ReplicaClient;
