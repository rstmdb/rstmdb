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
}

impl Instance {
    /// Creates a new instance.
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

/// Snapshot of an instance at a point in time.
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
        }
    }

    /// Restores an instance from a snapshot.
    pub fn to_instance(&self) -> Instance {
        Instance {
            id: self.instance_id.clone(),
            machine: self.machine.clone(),
            version: self.version,
            state: self.state.clone(),
            ctx: self.ctx.clone(),
            instance_state: InstanceState::Active,
            last_event_id: None,
            last_wal_offset: self.wal_offset,
            created_at: self.created_at,
            updated_at: self.created_at,
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
}
