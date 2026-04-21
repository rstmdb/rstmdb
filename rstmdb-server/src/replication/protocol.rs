//! Replication wire protocol messages.
//!
//! These messages are sent as JSON inside RCPX frames between primary and replica.

use rstmdb_wal::WalEntry;
use serde::{Deserialize, Serialize};

/// Replication protocol messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReplicationMessage {
    /// Replica → Primary: handshake with auth token and last known position.
    ReplicateAuth {
        auth_token: Option<String>,
        /// Replica's local last-applied sequence (legacy; kept for back-compat).
        last_sequence: u64,
        /// Highest **primary** WAL offset the replica has applied. Used by the
        /// primary's catchup to filter by offset rather than sequence —
        /// sequences can be non-monotonic in disk order under concurrent writes.
        /// Defaults to 0 for old replicas (falls back to sequence filtering).
        #[serde(default)]
        last_primary_offset: u64,
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
        /// Wall-clock timestamp (Unix ms) when the primary tailed this entry.
        /// Defaults to 0 for backward compatibility with older primaries.
        #[serde(default)]
        timestamp_ms: u64,
    },

    /// Replica → Primary: confirms entry was applied.
    ReplicateAck { sequence: u64 },

    /// Primary → Replica: periodic heartbeat with current sequence for lag calculation.
    ReplicateHeartbeat {
        primary_sequence: u64,
        /// Wall-clock timestamp (Unix ms) when the primary sent this heartbeat.
        timestamp_ms: u64,
        /// Wall-clock timestamp (Unix ms) of the primary's most recently written
        /// WAL entry. Used by the replica to compute time-based lag.
        /// Defaults to 0 for backward compatibility with older primaries.
        #[serde(default)]
        primary_latest_write_ts_ms: u64,
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
            last_primary_offset: 1099511628000,
        };
        let bytes = msg.to_bytes().unwrap();
        let parsed = ReplicationMessage::from_bytes(&bytes).unwrap();
        match parsed {
            ReplicationMessage::ReplicateAuth {
                auth_token,
                last_sequence,
                last_primary_offset,
            } => {
                assert_eq!(last_primary_offset, 1099511628000);
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
            timestamp_ms: 1_700_000_000_000,
        };
        let bytes = msg.to_bytes().unwrap();
        let parsed = ReplicationMessage::from_bytes(&bytes).unwrap();
        match parsed {
            ReplicationMessage::ReplicateEntry {
                sequence,
                offset,
                timestamp_ms,
                ..
            } => {
                assert_eq!(sequence, 1);
                assert_eq!(offset, 100);
                assert_eq!(timestamp_ms, 1_700_000_000_000);
            }
            _ => panic!("unexpected message type"),
        }
    }

    #[test]
    fn test_replicate_entry_backward_compat_no_timestamp() {
        // Old primary doesn't include timestamp_ms — should deserialize with 0.
        let json = r#"{
            "type": "replicate_entry",
            "sequence": 5,
            "offset": 200,
            "entry": {
                "type": "create_instance",
                "instance_id": "x",
                "machine": "m",
                "version": 1,
                "initial_state": "s",
                "initial_ctx": {},
                "idempotency_key": null
            }
        }"#;
        let parsed = ReplicationMessage::from_bytes(json.as_bytes()).unwrap();
        match parsed {
            ReplicationMessage::ReplicateEntry { timestamp_ms, .. } => {
                assert_eq!(timestamp_ms, 0);
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
            primary_latest_write_ts_ms: 1234567000,
        };
        let bytes = msg.to_bytes().unwrap();
        let parsed = ReplicationMessage::from_bytes(&bytes).unwrap();
        match parsed {
            ReplicationMessage::ReplicateHeartbeat {
                primary_sequence,
                timestamp_ms,
                primary_latest_write_ts_ms,
            } => {
                assert_eq!(primary_sequence, 500);
                assert_eq!(timestamp_ms, 1234567890);
                assert_eq!(primary_latest_write_ts_ms, 1234567000);
            }
            _ => panic!("unexpected message type"),
        }
    }
}
