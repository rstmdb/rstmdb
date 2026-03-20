//! Replication wire protocol messages.
//!
//! These messages are sent as JSON inside RCPX frames between primary and replica.

use rstmdb_wal::WalEntry;
use serde::{Deserialize, Serialize};

/// Replication protocol messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplicationMessage {
    /// Replica → Primary: handshake with auth token and last known sequence.
    ReplicateAuth {
        auth_token: Option<String>,
        last_sequence: u64,
    },

    /// Primary → Replica: confirms auth, reports current primary sequence.
    ReplicateSyncResponse {
        ok: bool,
        primary_sequence: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },

    /// Primary → Replica: a WAL entry to apply.
    ReplicateEntry {
        sequence: u64,
        offset: u64,
        entry: WalEntry,
    },

    /// Replica → Primary: confirms entry was applied.
    ReplicateAck { sequence: u64 },

    /// Primary → Replica: periodic heartbeat with current sequence for lag calculation.
    ReplicateHeartbeat {
        primary_sequence: u64,
        timestamp_ms: u64,
    },
}

impl ReplicationMessage {
    /// Returns true if this is an auth message (used for connection detection).
    pub fn is_auth(&self) -> bool {
        matches!(self, ReplicationMessage::ReplicateAuth { .. })
    }

    /// Serializes the message to JSON bytes.
    pub fn to_bytes(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(self)
    }

    /// Deserializes a message from JSON bytes.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, serde_json::Error> {
        serde_json::from_slice(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_replicate_auth_roundtrip() {
        let msg = ReplicationMessage::ReplicateAuth {
            auth_token: Some("test-token".to_string()),
            last_sequence: 42,
        };
        let bytes = msg.to_bytes().unwrap();
        let parsed = ReplicationMessage::from_bytes(&bytes).unwrap();
        match parsed {
            ReplicationMessage::ReplicateAuth {
                auth_token,
                last_sequence,
            } => {
                assert_eq!(auth_token, Some("test-token".to_string()));
                assert_eq!(last_sequence, 42);
            }
            _ => panic!("unexpected message type"),
        }
    }

    #[test]
    fn test_replicate_entry_roundtrip() {
        let msg = ReplicationMessage::ReplicateEntry {
            sequence: 1,
            offset: 100,
            entry: WalEntry::CreateInstance {
                instance_id: "test".to_string(),
                machine: "order".to_string(),
                version: 1,
                initial_state: "created".to_string(),
                initial_ctx: serde_json::json!({}),
                idempotency_key: None,
            },
        };
        let bytes = msg.to_bytes().unwrap();
        let parsed = ReplicationMessage::from_bytes(&bytes).unwrap();
        match parsed {
            ReplicationMessage::ReplicateEntry {
                sequence, offset, ..
            } => {
                assert_eq!(sequence, 1);
                assert_eq!(offset, 100);
            }
            _ => panic!("unexpected message type"),
        }
    }

    #[test]
    fn test_replicate_ack_roundtrip() {
        let msg = ReplicationMessage::ReplicateAck { sequence: 99 };
        let bytes = msg.to_bytes().unwrap();
        let parsed = ReplicationMessage::from_bytes(&bytes).unwrap();
        match parsed {
            ReplicationMessage::ReplicateAck { sequence } => {
                assert_eq!(sequence, 99);
            }
            _ => panic!("unexpected message type"),
        }
    }

    #[test]
    fn test_heartbeat_roundtrip() {
        let msg = ReplicationMessage::ReplicateHeartbeat {
            primary_sequence: 500,
            timestamp_ms: 1234567890,
        };
        let bytes = msg.to_bytes().unwrap();
        let parsed = ReplicationMessage::from_bytes(&bytes).unwrap();
        match parsed {
            ReplicationMessage::ReplicateHeartbeat {
                primary_sequence,
                timestamp_ms,
            } => {
                assert_eq!(primary_sequence, 500);
                assert_eq!(timestamp_ms, 1234567890);
            }
            _ => panic!("unexpected message type"),
        }
    }
}
