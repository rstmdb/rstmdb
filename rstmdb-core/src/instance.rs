//! Instance state management.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Instance state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstanceState {
    /// Instance is active.
    #[default]
    Active,
    /// Instance is soft-deleted.
    Deleted,
}

/// A state machine instance.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Instance {
    /// Unique instance ID.
    pub id: String,

    /// Machine name.
    pub machine: String,

    /// Machine version.
    pub version: u32,

    /// Current state in the machine.
    pub state: String,

    /// Instance context (mutable data).
    pub ctx: Value,

    /// Instance lifecycle state.
    pub instance_state: InstanceState,

    /// Last event ID applied (optional, user-provided).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<String>,

    /// Last WAL offset.
    pub last_wal_offset: u64,

    /// Creation timestamp (Unix millis).
    pub created_at: i64,

    /// Last update timestamp (Unix millis).
    pub updated_at: i64,

    /// Idempotency key used by the CREATE_INSTANCE request that created this
    /// instance. A retry with the same key returns this instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_idempotency_key: Option<String>,

    /// Canonical request fingerprint paired with `create_idempotency_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_request_hash: Option<String>,
}

impl Instance {
    /// Creates a new instance.
    ///
    /// # Examples
    ///
    /// ```
    /// use rstmdb_core::Instance;
    /// use serde_json::json;
    ///
    /// let instance = Instance::new("order-001", "order", 1, "created", json!({}), 0);
    /// assert_eq!(instance.id, "order-001");
    /// assert_eq!(instance.state, "created");
    /// assert!(instance.is_active());
    /// ```
    pub fn new(
        id: impl Into<String>,
        machine: impl Into<String>,
        version: u32,
        initial_state: impl Into<String>,
        initial_ctx: Value,
        wal_offset: u64,
    ) -> Self {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;

        Self {
            id: id.into(),
            machine: machine.into(),
            version,
            state: initial_state.into(),
            ctx: initial_ctx,
            instance_state: InstanceState::Active,
            last_event_id: None,
            last_wal_offset: wal_offset,
            created_at: now,
            updated_at: now,
            create_idempotency_key: None,
            create_request_hash: None,
        }
    }

    /// Updates the instance state after applying an event.
    pub fn apply_transition(
        &mut self,
        new_state: impl Into<String>,
        new_ctx: Value,
        event_id: Option<String>,
        wal_offset: u64,
    ) {
        self.state = new_state.into();
        self.ctx = new_ctx;
        self.last_event_id = event_id;
        self.last_wal_offset = wal_offset;
        self.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
    }

    /// Marks the instance as deleted.
    ///
    /// # Examples
    ///
    /// ```
    /// use rstmdb_core::Instance;
    /// use serde_json::json;
    ///
    /// let mut instance = Instance::new("i1", "order", 1, "created", json!({}), 0);
    /// assert!(instance.is_active());
    ///
    /// instance.soft_delete(1);
    /// assert!(instance.is_deleted());
    /// assert!(!instance.is_active());
    /// ```
    pub fn soft_delete(&mut self, wal_offset: u64) {
        self.instance_state = InstanceState::Deleted;
        self.last_wal_offset = wal_offset;
        self.updated_at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64;
    }

    /// Returns true if the instance is active.
    pub fn is_active(&self) -> bool {
        self.instance_state == InstanceState::Active
    }

    /// Returns true if the instance is deleted.
    pub fn is_deleted(&self) -> bool {
        self.instance_state == InstanceState::Deleted
    }
}

/// A cached APPLY_EVENT result, persisted so idempotency survives restarts
/// and WAL compaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdempotencyRecord {
    /// Idempotency key supplied by the client.
    pub key: String,
    /// State before the event.
    pub from_state: String,
    /// State after the event.
    pub to_state: String,
    /// Context after the event.
    pub ctx: Value,
    /// WAL offset of the original APPLY_EVENT.
    pub wal_offset: u64,
    /// WAL sequence of the original APPLY_EVENT.
    #[serde(default)]
    pub sequence: u64,
    /// Canonical request fingerprint (`event`, `event_id`, `payload`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_hash: Option<String>,
}

/// Snapshot of an instance at a point in time.
///
/// Fields added after the initial format use `#[serde(default)]` so older
/// snapshot files remain readable.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceSnapshot {
    /// Snapshot ID.
    pub snapshot_id: String,

    /// Instance ID.
    pub instance_id: String,

    /// Machine name.
    pub machine: String,

    /// Machine version.
    pub version: u32,

    /// State at snapshot time.
    pub state: String,

    /// Context at snapshot time.
    pub ctx: Value,

    /// WAL offset at snapshot time.
    pub wal_offset: u64,

    /// Snapshot creation timestamp.
    pub created_at: i64,

    /// Whether the instance was soft-deleted (tombstone snapshot).
    #[serde(default)]
    pub deleted: bool,

    /// Last event ID applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<String>,

    /// Idempotency key of the creating request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_idempotency_key: Option<String>,

    /// Canonical request fingerprint of the creating request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub create_request_hash: Option<String>,

    /// Cached APPLY_EVENT results for this instance.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub idempotency: Vec<IdempotencyRecord>,
}

impl InstanceSnapshot {
    /// Creates a snapshot from an instance.
    pub fn from_instance(instance: &Instance, snapshot_id: impl Into<String>) -> Self {
        Self {
            snapshot_id: snapshot_id.into(),
            instance_id: instance.id.clone(),
            machine: instance.machine.clone(),
            version: instance.version,
            state: instance.state.clone(),
            ctx: instance.ctx.clone(),
            wal_offset: instance.last_wal_offset,
            created_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64,
            deleted: instance.is_deleted(),
            last_event_id: instance.last_event_id.clone(),
            create_idempotency_key: instance.create_idempotency_key.clone(),
            create_request_hash: instance.create_request_hash.clone(),
            idempotency: Vec::new(),
        }
    }

    /// Attaches the instance's cached idempotency results.
    pub fn with_idempotency(mut self, records: Vec<IdempotencyRecord>) -> Self {
        self.idempotency = records;
        self
    }

    /// Restores an instance from a snapshot.
    pub fn to_instance(&self) -> Instance {
        Instance {
            id: self.instance_id.clone(),
            machine: self.machine.clone(),
            version: self.version,
            state: self.state.clone(),
            ctx: self.ctx.clone(),
            instance_state: if self.deleted {
                InstanceState::Deleted
            } else {
                InstanceState::Active
            },
            last_event_id: self.last_event_id.clone(),
            last_wal_offset: self.wal_offset,
            created_at: self.created_at,
            updated_at: self.created_at,
            create_idempotency_key: self.create_idempotency_key.clone(),
            create_request_hash: self.create_request_hash.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_instance_creation() {
        let instance = Instance::new("i-1", "order", 1, "created", json!({}), 0);
        assert_eq!(instance.id, "i-1");
        assert_eq!(instance.machine, "order");
        assert_eq!(instance.state, "created");
        assert!(instance.is_active());
    }

    #[test]
    fn test_instance_transition() {
        let mut instance = Instance::new("i-1", "order", 1, "created", json!({}), 0);
        instance.apply_transition("paid", json!({"amount": 100}), Some("e-1".to_string()), 1);

        assert_eq!(instance.state, "paid");
        assert_eq!(instance.ctx, json!({"amount": 100}));
        assert_eq!(instance.last_event_id, Some("e-1".to_string()));
        assert_eq!(instance.last_wal_offset, 1);
    }

    #[test]
    fn test_instance_soft_delete() {
        let mut instance = Instance::new("i-1", "order", 1, "created", json!({}), 0);
        instance.soft_delete(1);

        assert!(instance.is_deleted());
        assert!(!instance.is_active());
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let instance = Instance::new("i-1", "order", 1, "paid", json!({"amount": 100}), 5);
        let snapshot = InstanceSnapshot::from_instance(&instance, "snap-1");

        assert_eq!(snapshot.instance_id, "i-1");
        assert_eq!(snapshot.state, "paid");
        assert_eq!(snapshot.wal_offset, 5);

        let restored = snapshot.to_instance();
        assert_eq!(restored.id, instance.id);
        assert_eq!(restored.state, instance.state);
        assert_eq!(restored.ctx, instance.ctx);
    }

    #[test]
    fn test_snapshot_preserves_tombstone_event_id_and_keys() {
        let mut instance = Instance::new("i-1", "order", 1, "paid", json!({}), 5);
        instance.apply_transition("shipped", json!({}), Some("e-9".to_string()), 7);
        instance.create_idempotency_key = Some("create-1".to_string());
        instance.soft_delete(9);

        let record = IdempotencyRecord {
            key: "k".to_string(),
            from_state: "paid".to_string(),
            to_state: "shipped".to_string(),
            ctx: json!({}),
            wal_offset: 7,
            sequence: 3,
            request_hash: Some("hash-1".to_string()),
        };
        let snapshot = InstanceSnapshot::from_instance(&instance, "snap-1")
            .with_idempotency(vec![record.clone()]);
        let json = serde_json::to_string(&snapshot).unwrap();
        let decoded: InstanceSnapshot = serde_json::from_str(&json).unwrap();
        let restored = decoded.to_instance();

        assert!(restored.is_deleted());
        assert_eq!(restored.last_event_id.as_deref(), Some("e-9"));
        assert_eq!(restored.create_idempotency_key.as_deref(), Some("create-1"));
        assert_eq!(decoded.idempotency, vec![record]);
    }

    #[test]
    fn test_snapshot_reads_legacy_format() {
        let legacy = r#"{"snapshot_id":"s","instance_id":"i","machine":"m","version":1,
            "state":"a","ctx":{},"wal_offset":3,"created_at":0}"#;
        let snapshot: InstanceSnapshot = serde_json::from_str(legacy).unwrap();
        assert!(!snapshot.deleted);
        assert!(snapshot.idempotency.is_empty());
        assert!(snapshot.to_instance().is_active());
    }
}
