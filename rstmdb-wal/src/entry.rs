//! WAL entry types.
//!
//! Each WAL record has the following on-disk format:
//!
//! ```text
//! +----------+----------+----------+----------+----------+----------+
//! | magic    | type     | flags    | reserved | length   | crc32c   |
//! | 4 bytes  | 1 byte   | 1 byte   | 2 bytes  | 4 bytes  | 4 bytes  |
//! +----------+----------+----------+----------+----------+----------+
//! | sequence_number     | payload                                   |
//! | 8 bytes             | length bytes                              |
//! +---------------------+-------------------------------------------+
//! ```

use crate::error::WalError;
use crate::RECORD_HEADER_SIZE;
use bytes::{Buf, BufMut, Bytes, BytesMut};
use serde::{Deserialize, Serialize};

/// Magic bytes for WAL records: "WLOG"
pub const WAL_MAGIC: [u8; 4] = *b"WLOG";

/// Maximum record payload size (16 MiB).
pub const MAX_RECORD_SIZE: usize = 16 * 1024 * 1024;

/// Type of WAL entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum WalEntryType {
    /// Machine definition created or updated.
    PutMachine = 1,
    /// Instance created.
    CreateInstance = 2,
    /// Event applied to instance.
    ApplyEvent = 3,
    /// Instance deleted (soft).
    DeleteInstance = 4,
    /// Snapshot marker.
    Snapshot = 5,
    /// Checkpoint marker (for recovery).
    Checkpoint = 6,
    /// No-op (for padding/alignment).
    Noop = 255,
}

impl TryFrom<u8> for WalEntryType {
    type Error = WalError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(WalEntryType::PutMachine),
            2 => Ok(WalEntryType::CreateInstance),
            3 => Ok(WalEntryType::ApplyEvent),
            4 => Ok(WalEntryType::DeleteInstance),
            5 => Ok(WalEntryType::Snapshot),
            6 => Ok(WalEntryType::Checkpoint),
            255 => Ok(WalEntryType::Noop),
            _ => Err(WalError::InvalidHeader {
                offset: 0,
                reason: format!("unknown entry type: {}", value),
            }),
        }
    }
}

/// A parsed WAL record header.
#[derive(Debug, Clone)]
pub struct WalRecordHeader {
    pub entry_type: WalEntryType,
    pub flags: u8,
    pub payload_len: u32,
    pub crc32c: u32,
    pub sequence: u64,
}

/// A complete WAL record (header + payload).
#[derive(Debug, Clone)]
pub struct WalRecord {
    pub header: WalRecordHeader,
    pub payload: Bytes,
}

impl WalRecord {
    /// Creates a new WAL record.
    pub fn new(entry_type: WalEntryType, sequence: u64, payload: Bytes) -> Self {
        let crc = crc32c::crc32c(&payload);
        Self {
            header: WalRecordHeader {
                entry_type,
                flags: 0,
                payload_len: payload.len() as u32,
                crc32c: crc,
                sequence,
            },
            payload,
        }
    }

    /// Encodes the record into bytes.
    pub fn encode(&self) -> Result<BytesMut, WalError> {
        if self.payload.len() > MAX_RECORD_SIZE {
            return Err(WalError::RecordTooLarge {
                size: self.payload.len(),
                max: MAX_RECORD_SIZE,
            });
        }

        let total_size = RECORD_HEADER_SIZE + self.payload.len();
        let mut buf = BytesMut::with_capacity(total_size);

        // Magic (4 bytes)
        buf.put_slice(&WAL_MAGIC);

        // Type (1 byte)
        buf.put_u8(self.header.entry_type as u8);

        // Flags (1 byte)
        buf.put_u8(self.header.flags);

        // Reserved (2 bytes)
        buf.put_u16(0);

        // Payload length (4 bytes)
        buf.put_u32(self.header.payload_len);

        // CRC32C (4 bytes)
        buf.put_u32(self.header.crc32c);

        // Sequence number (8 bytes)
        buf.put_u64(self.header.sequence);

        // Payload
        buf.put_slice(&self.payload);

        Ok(buf)
    }

    /// Decodes a record from bytes.
    pub fn decode(buf: &mut BytesMut, offset: u64) -> Result<Option<Self>, WalError> {
        if buf.len() < RECORD_HEADER_SIZE {
            return Ok(None);
        }

        // Peek at header
        let magic: [u8; 4] = buf[0..4].try_into().unwrap();
        if magic != WAL_MAGIC {
            // Could be EOF padding or corruption
            if magic == [0, 0, 0, 0] {
                return Ok(None);
            }
            return Err(WalError::InvalidHeader {
                offset,
                reason: format!("invalid magic: {:?}", magic),
            });
        }

        let entry_type = WalEntryType::try_from(buf[4]).map_err(|_| WalError::InvalidHeader {
            offset,
            reason: format!("unknown entry type: {}", buf[4]),
        })?;

        let flags = buf[5];
        // reserved: buf[6..8]
        let payload_len = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]) as usize;
        let crc_expected = u32::from_be_bytes([buf[12], buf[13], buf[14], buf[15]]);
        let sequence = u64::from_be_bytes([
            buf[16], buf[17], buf[18], buf[19], buf[20], buf[21], buf[22], buf[23],
        ]);

        if payload_len > MAX_RECORD_SIZE {
            return Err(WalError::RecordTooLarge {
                size: payload_len,
                max: MAX_RECORD_SIZE,
            });
        }

        let total_len = RECORD_HEADER_SIZE + payload_len;
        if buf.len() < total_len {
            return Ok(None);
        }

        // Consume header
        buf.advance(RECORD_HEADER_SIZE);

        // Read payload
        let payload = buf.split_to(payload_len).freeze();

        // Validate CRC
        let crc_actual = crc32c::crc32c(&payload);
        if crc_actual != crc_expected {
            return Err(WalError::CorruptedRecord {
                offset,
                expected: crc_expected,
                actual: crc_actual,
            });
        }

        Ok(Some(Self {
            header: WalRecordHeader {
                entry_type,
                flags,
                payload_len: payload_len as u32,
                crc32c: crc_expected,
                sequence,
            },
            payload,
        }))
    }

    /// Returns the total size of this record on disk.
    pub fn disk_size(&self) -> usize {
        RECORD_HEADER_SIZE + self.payload.len()
    }
}

/// Typed WAL entry with deserialized payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WalEntry {
    PutMachine {
        machine: String,
        version: u32,
        definition_hash: String,
        /// The full definition JSON for replay.
        #[serde(default)]
        definition: serde_json::Value,
    },
    CreateInstance {
        instance_id: String,
        machine: String,
        version: u32,
        initial_state: String,
        #[serde(default)]
        initial_ctx: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
    },
    ApplyEvent {
        instance_id: String,
        event: String,
        from_state: String,
        to_state: String,
        #[serde(default)]
        payload: serde_json::Value,
        #[serde(default)]
        ctx: serde_json::Value,
        #[serde(skip_serializing_if = "Option::is_none")]
        event_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
    },
    DeleteInstance {
        instance_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        idempotency_key: Option<String>,
    },
    Snapshot {
        instance_id: String,
        snapshot_id: String,
        state: String,
        ctx: serde_json::Value,
    },
    Checkpoint {
        timestamp: i64,
    },
}

impl WalEntry {
    /// Returns the entry type for this entry.
    pub fn entry_type(&self) -> WalEntryType {
        match self {
            WalEntry::PutMachine { .. } => WalEntryType::PutMachine,
            WalEntry::CreateInstance { .. } => WalEntryType::CreateInstance,
            WalEntry::ApplyEvent { .. } => WalEntryType::ApplyEvent,
            WalEntry::DeleteInstance { .. } => WalEntryType::DeleteInstance,
            WalEntry::Snapshot { .. } => WalEntryType::Snapshot,
            WalEntry::Checkpoint { .. } => WalEntryType::Checkpoint,
        }
    }

    /// Returns the instance ID if this entry is instance-related.
    pub fn instance_id(&self) -> Option<&str> {
        match self {
            WalEntry::CreateInstance { instance_id, .. }
            | WalEntry::ApplyEvent { instance_id, .. }
            | WalEntry::DeleteInstance { instance_id, .. }
            | WalEntry::Snapshot { instance_id, .. } => Some(instance_id),
            _ => None,
        }
    }

    /// Returns the idempotency key if present.
    pub fn idempotency_key(&self) -> Option<&str> {
        match self {
            WalEntry::CreateInstance {
                idempotency_key, ..
            }
            | WalEntry::ApplyEvent {
                idempotency_key, ..
            }
            | WalEntry::DeleteInstance {
                idempotency_key, ..
            } => idempotency_key.as_deref(),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_record_roundtrip() {
        let payload = Bytes::from(r#"{"test":"data"}"#);
        let record = WalRecord::new(WalEntryType::ApplyEvent, 42, payload.clone());

        let encoded = record.encode().unwrap();
        let mut buf = encoded;
        let decoded = WalRecord::decode(&mut buf, 0).unwrap().unwrap();

        assert_eq!(decoded.header.entry_type, WalEntryType::ApplyEvent);
        assert_eq!(decoded.header.sequence, 42);
        assert_eq!(decoded.payload, payload);
    }

    #[test]
    fn test_corrupted_record_detection() {
        let payload = Bytes::from(r#"{"test":"data"}"#);
        let record = WalRecord::new(WalEntryType::ApplyEvent, 1, payload);
        let mut encoded = record.encode().unwrap();

        // Corrupt the payload
        let len = encoded.len();
        encoded[len - 1] ^= 0xFF;

        let result = WalRecord::decode(&mut encoded, 0);
        assert!(matches!(result, Err(WalError::CorruptedRecord { .. })));
    }

    #[test]
    fn test_entry_serialization() {
        let entry = WalEntry::ApplyEvent {
            instance_id: "i-1".to_string(),
            event: "PAY".to_string(),
            from_state: "created".to_string(),
            to_state: "paid".to_string(),
            payload: serde_json::json!({"amount": 100}),
            ctx: serde_json::json!({}),
            event_id: Some("e-1".to_string()),
            idempotency_key: Some("k-1".to_string()),
        };

        let json = serde_json::to_string(&entry).unwrap();
        let parsed: WalEntry = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.entry_type(), WalEntryType::ApplyEvent);
        assert_eq!(parsed.instance_id(), Some("i-1"));
    }

    #[test]
    fn test_entry_type_conversion() {
        assert_eq!(
            WalEntryType::try_from(1u8).unwrap(),
            WalEntryType::PutMachine
        );
        assert_eq!(
            WalEntryType::try_from(2u8).unwrap(),
            WalEntryType::CreateInstance
        );
        assert_eq!(
            WalEntryType::try_from(3u8).unwrap(),
            WalEntryType::ApplyEvent
        );
        assert_eq!(
            WalEntryType::try_from(4u8).unwrap(),
            WalEntryType::DeleteInstance
        );
        assert_eq!(WalEntryType::try_from(5u8).unwrap(), WalEntryType::Snapshot);
        assert_eq!(
            WalEntryType::try_from(6u8).unwrap(),
            WalEntryType::Checkpoint
        );
        assert_eq!(WalEntryType::try_from(255u8).unwrap(), WalEntryType::Noop);
        assert!(WalEntryType::try_from(100u8).is_err());
    }

    #[test]
    fn test_record_too_large() {
        let huge_payload = Bytes::from(vec![0u8; MAX_RECORD_SIZE + 1]);
        let record = WalRecord::new(WalEntryType::ApplyEvent, 1, huge_payload);
        let result = record.encode();
        assert!(matches!(result, Err(WalError::RecordTooLarge { .. })));
    }

    #[test]
    fn test_record_disk_size() {
        let payload = Bytes::from(r#"{"test":"data"}"#);
        let record = WalRecord::new(WalEntryType::ApplyEvent, 1, payload.clone());
        assert_eq!(record.disk_size(), RECORD_HEADER_SIZE + payload.len());
    }

    #[test]
    fn test_incomplete_record() {
        // Less than header size
        let mut buf = BytesMut::from(&b"WLOG"[..]);
        let result = WalRecord::decode(&mut buf, 0);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_eof_padding() {
        // All zeros indicates EOF
        let mut buf = BytesMut::from(&[0u8; 24][..]);
        let result = WalRecord::decode(&mut buf, 0);
        assert!(result.unwrap().is_none());
    }

    #[test]
    fn test_invalid_magic() {
        let mut buf = BytesMut::from(&b"BADX\x01\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00"[..]);
        let result = WalRecord::decode(&mut buf, 0);
        assert!(matches!(result, Err(WalError::InvalidHeader { .. })));
    }

    #[test]
    fn test_all_entry_types() {
        let put_machine = WalEntry::PutMachine {
            machine: "test".to_string(),
            version: 1,
            definition_hash: "hash".to_string(),
            definition: serde_json::json!({"states": ["init"], "initial": "init"}),
        };
        assert_eq!(put_machine.entry_type(), WalEntryType::PutMachine);
        assert!(put_machine.instance_id().is_none());
        assert!(put_machine.idempotency_key().is_none());

        let create = WalEntry::CreateInstance {
            instance_id: "i-1".to_string(),
            machine: "test".to_string(),
            version: 1,
            initial_state: "init".to_string(),
            initial_ctx: serde_json::json!({}),
            idempotency_key: Some("key-1".to_string()),
        };
        assert_eq!(create.entry_type(), WalEntryType::CreateInstance);
        assert_eq!(create.instance_id(), Some("i-1"));
        assert_eq!(create.idempotency_key(), Some("key-1"));

        let delete = WalEntry::DeleteInstance {
            instance_id: "i-1".to_string(),
            idempotency_key: None,
        };
        assert_eq!(delete.entry_type(), WalEntryType::DeleteInstance);
        assert!(delete.idempotency_key().is_none());

        let snapshot = WalEntry::Snapshot {
            instance_id: "i-1".to_string(),
            snapshot_id: "snap-1".to_string(),
            state: "active".to_string(),
            ctx: serde_json::json!({}),
        };
        assert_eq!(snapshot.entry_type(), WalEntryType::Snapshot);
        assert_eq!(snapshot.instance_id(), Some("i-1"));

        let checkpoint = WalEntry::Checkpoint { timestamp: 12345 };
        assert_eq!(checkpoint.entry_type(), WalEntryType::Checkpoint);
        assert!(checkpoint.instance_id().is_none());
    }
}
