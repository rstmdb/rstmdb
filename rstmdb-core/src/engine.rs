//! State machine engine - coordinates definitions, instances, and WAL.

use crate::definition::{MachineDefinition, State};
use crate::error::CoreError;
use crate::guard::GuardEvaluator;
use crate::instance::Instance;
use dashmap::DashMap;
use parking_lot::RwLock;
use rstmdb_wal::{Wal, WalConfig, WalEntry};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

/// Result of applying an event.
#[derive(Debug, Clone)]
pub struct ApplyResult {
    pub from_state: String,
    pub to_state: String,
    pub ctx: Value,
    pub wal_offset: u64,
    pub sequence: u64,
    pub applied: bool,
}

/// The state machine engine.
pub struct StateMachineEngine {
    /// Machine definitions indexed by (name, version).
    definitions: DashMap<(String, u32), Arc<MachineDefinition>>,

    /// Instances indexed by ID.
    instances: DashMap<String, RwLock<Instance>>,

    /// Idempotency cache: (instance_id, idempotency_key) -> result.
    idempotency_cache: DashMap<(String, String), ApplyResult>,

    /// Write-ahead log.
    wal: Arc<Wal>,
}

impl StateMachineEngine {
    /// Creates a new engine with the given WAL configuration.
    /// Replays WAL entries to restore state.
    pub fn new(wal_config: WalConfig) -> Result<Self, CoreError> {
        let wal = Arc::new(Wal::open(wal_config)?);

        let engine = Self {
            definitions: DashMap::new(),
            instances: DashMap::new(),
            idempotency_cache: DashMap::new(),
            wal,
        };

        // Replay WAL to restore state
        engine.replay_wal()?;

        Ok(engine)
    }

    /// Creates a new engine with an existing WAL.
    /// Replays WAL entries to restore state.
    pub fn with_wal(wal: Arc<Wal>) -> Result<Self, CoreError> {
        let engine = Self {
            definitions: DashMap::new(),
            instances: DashMap::new(),
            idempotency_cache: DashMap::new(),
            wal,
        };

        engine.replay_wal()?;

        Ok(engine)
    }

    /// Replays all WAL entries to restore state.
    fn replay_wal(&self) -> Result<(), CoreError> {
        use rstmdb_wal::WalOffset;

        // Read all entries from the beginning
        let entries = self.wal.read_from(WalOffset::from_u64(0), None)?;
        let entry_count = entries.len();

        for (_seq, offset, entry) in entries {
            self.replay_entry(offset.as_u64(), entry)?;
        }

        if entry_count > 0 {
            tracing::info!(
                "WAL replay complete: {} entries, {} machines, {} instances",
                entry_count,
                self.definitions.len(),
                self.instances.len()
            );
        }

        Ok(())
    }

    /// Replays a single WAL entry.
    fn replay_entry(&self, offset: u64, entry: WalEntry) -> Result<(), CoreError> {
        match entry {
            WalEntry::PutMachine {
                machine,
                version,
                definition,
                ..
            } => {
                // Skip if definition is empty (old format)
                if definition.is_null() {
                    tracing::warn!(
                        "Cannot replay PutMachine for {}:{} - no definition in WAL entry",
                        machine,
                        version
                    );
                    return Ok(());
                }

                let key = (machine.clone(), version);
                if !self.definitions.contains_key(&key) {
                    match MachineDefinition::from_json(&machine, version, &definition) {
                        Ok(def) => {
                            self.definitions.insert(key, Arc::new(def));
                        }
                        Err(e) => {
                            tracing::warn!(
                                "Failed to replay machine {}:{}: {}",
                                machine,
                                version,
                                e
                            );
                        }
                    }
                }
            }

            WalEntry::CreateInstance {
                instance_id,
                machine,
                version,
                initial_state,
                initial_ctx,
                ..
            } => {
                if !self.instances.contains_key(&instance_id) {
                    let instance = Instance::new(
                        instance_id.clone(),
                        machine,
                        version,
                        initial_state,
                        initial_ctx,
                        offset,
                    );
                    self.instances
                        .insert(instance_id, parking_lot::RwLock::new(instance));
                }
            }

            WalEntry::ApplyEvent {
                instance_id,
                to_state,
                ctx,
                event_id,
                ..
            } => {
                if let Some(instance_lock) = self.instances.get(&instance_id) {
                    let mut instance = instance_lock.write();
                    instance.state = to_state;
                    instance.ctx = ctx;
                    instance.last_wal_offset = offset;
                    if let Some(id) = event_id {
                        instance.last_event_id = Some(id);
                    }
                }
            }

            WalEntry::DeleteInstance { instance_id, .. } => {
                if let Some(instance_lock) = self.instances.get(&instance_id) {
                    let mut instance = instance_lock.write();
                    instance.instance_state = crate::instance::InstanceState::Deleted;
                    instance.last_wal_offset = offset;
                }
            }

            WalEntry::Snapshot { .. } | WalEntry::Checkpoint { .. } => {
                // These don't affect in-memory state
            }
        }

        Ok(())
    }

    // =========================================================================
    // Machine Definition Management
    // =========================================================================

    /// Registers a machine definition.
    pub fn put_machine(
        &self,
        name: &str,
        version: u32,
        definition_json: &Value,
    ) -> Result<(String, bool), CoreError> {
        let key = (name.to_string(), version);

        // Check if already exists
        if let Some(existing) = self.definitions.get(&key) {
            let new_def = MachineDefinition::from_json(name, version, definition_json)?;
            if existing.checksum == new_def.checksum {
                // Idempotent success
                return Ok((existing.checksum.clone(), false));
            } else {
                return Err(CoreError::MachineVersionExists {
                    machine: name.to_string(),
                    version,
                });
            }
        }

        // Parse and validate
        let definition = MachineDefinition::from_json(name, version, definition_json)?;
        let checksum = definition.checksum.clone();

        // Write to WAL (include full definition for replay)
        let entry = WalEntry::PutMachine {
            machine: name.to_string(),
            version,
            definition_hash: checksum.clone(),
            definition: definition_json.clone(),
        };
        self.wal.append(&entry)?;

        // Store
        self.definitions.insert(key, Arc::new(definition));

        Ok((checksum, true))
    }

    /// Gets a machine definition.
    pub fn get_machine(
        &self,
        name: &str,
        version: u32,
    ) -> Result<Arc<MachineDefinition>, CoreError> {
        self.definitions
            .get(&(name.to_string(), version))
            .map(|r| r.clone())
            .ok_or_else(|| CoreError::MachineVersionNotFound {
                machine: name.to_string(),
                version,
            })
    }

    /// Lists all machines and their versions.
    pub fn list_machines(&self) -> HashMap<String, Vec<u32>> {
        let mut result: HashMap<String, Vec<u32>> = HashMap::new();
        for entry in self.definitions.iter() {
            let (name, version) = entry.key();
            result.entry(name.clone()).or_default().push(*version);
        }
        for versions in result.values_mut() {
            versions.sort();
        }
        result
    }

    // =========================================================================
    // Instance Management
    // =========================================================================

    /// Creates a new instance.
    pub fn create_instance(
        &self,
        instance_id: &str,
        machine: &str,
        version: u32,
        initial_ctx: Value,
        idempotency_key: Option<&str>,
    ) -> Result<(Instance, u64), CoreError> {
        // Check idempotency
        if let Some(key) = idempotency_key {
            let cache_key = (instance_id.to_string(), key.to_string());
            if self.idempotency_cache.contains_key(&cache_key) {
                // Already created, return existing
                if let Some(instance_lock) = self.instances.get(instance_id) {
                    let instance = instance_lock.read();
                    return Ok((instance.clone(), instance.last_wal_offset));
                }
            }
        }

        // Check if instance already exists
        if self.instances.contains_key(instance_id) {
            return Err(CoreError::InstanceExists {
                instance_id: instance_id.to_string(),
            });
        }

        // Get machine definition
        let definition = self.get_machine(machine, version)?;

        // Write to WAL
        let entry = WalEntry::CreateInstance {
            instance_id: instance_id.to_string(),
            machine: machine.to_string(),
            version,
            initial_state: definition.initial.as_str().to_string(),
            initial_ctx: initial_ctx.clone(),
            idempotency_key: idempotency_key.map(|s| s.to_string()),
        };
        let (sequence, offset) = self.wal.append(&entry)?;

        // Create instance
        let instance = Instance::new(
            instance_id,
            machine,
            version,
            definition.initial.as_str(),
            initial_ctx,
            offset.as_u64(),
        );

        // Store
        self.instances
            .insert(instance_id.to_string(), RwLock::new(instance.clone()));

        Ok((instance, sequence))
    }

    /// Gets an instance by ID.
    pub fn get_instance(&self, instance_id: &str) -> Result<Instance, CoreError> {
        self.instances
            .get(instance_id)
            .map(|r| r.read().clone())
            .ok_or_else(|| CoreError::InstanceNotFound {
                instance_id: instance_id.to_string(),
            })
    }

    /// Applies an event to an instance.
    #[allow(clippy::too_many_arguments)]
    pub fn apply_event(
        &self,
        instance_id: &str,
        event: &str,
        payload: Value,
        expected_state: Option<&str>,
        expected_wal_offset: Option<u64>,
        event_id: Option<&str>,
        idempotency_key: Option<&str>,
    ) -> Result<ApplyResult, CoreError> {
        // Check idempotency cache
        if let Some(key) = idempotency_key {
            let cache_key = (instance_id.to_string(), key.to_string());
            if let Some(result) = self.idempotency_cache.get(&cache_key) {
                return Ok(result.clone());
            }
        }

        // Get instance (with write lock for the duration)
        let instance_lock =
            self.instances
                .get(instance_id)
                .ok_or_else(|| CoreError::InstanceNotFound {
                    instance_id: instance_id.to_string(),
                })?;

        let mut instance = instance_lock.write();

        // Check expected state
        if let Some(expected) = expected_state {
            if instance.state != expected {
                return Err(CoreError::StateConflict {
                    expected: expected.to_string(),
                    actual: instance.state.clone(),
                });
            }
        }

        // Check expected WAL offset
        if let Some(expected) = expected_wal_offset {
            if instance.last_wal_offset != expected {
                return Err(CoreError::WalOffsetConflict {
                    expected,
                    actual: instance.last_wal_offset,
                });
            }
        }

        // Get machine definition
        let definition = self.get_machine(&instance.machine, instance.version)?;

        // Look up transition
        let current_state = State::from(instance.state.as_str());
        let (to_state, guard) = definition
            .get_transition(&current_state, event)
            .ok_or_else(|| CoreError::InvalidTransition {
                state: instance.state.clone(),
                event: event.to_string(),
            })?;

        // Evaluate guard
        if !GuardEvaluator::evaluate_opt(guard, &instance.ctx) {
            return Err(CoreError::GuardFailed {
                reason: format!(
                    "guard failed for transition '{}' -> '{}' on event '{}'",
                    instance.state,
                    to_state.as_str(),
                    event
                ),
            });
        }

        // Merge payload into context
        let new_ctx = merge_ctx(&instance.ctx, &payload);

        // Write to WAL
        let entry = WalEntry::ApplyEvent {
            instance_id: instance_id.to_string(),
            event: event.to_string(),
            from_state: instance.state.clone(),
            to_state: to_state.as_str().to_string(),
            payload: payload.clone(),
            ctx: new_ctx.clone(),
            event_id: event_id.map(|s| s.to_string()),
            idempotency_key: idempotency_key.map(|s| s.to_string()),
        };
        let (sequence, offset) = self.wal.append(&entry)?;

        // Build result
        let result = ApplyResult {
            from_state: instance.state.clone(),
            to_state: to_state.as_str().to_string(),
            ctx: new_ctx.clone(),
            wal_offset: offset.as_u64(),
            sequence,
            applied: true,
        };

        // Update instance
        instance.apply_transition(
            to_state.as_str(),
            new_ctx,
            event_id.map(|s| s.to_string()),
            offset.as_u64(),
        );

        // Cache idempotency
        if let Some(key) = idempotency_key {
            let cache_key = (instance_id.to_string(), key.to_string());
            self.idempotency_cache.insert(cache_key, result.clone());
        }

        Ok(result)
    }

    /// Deletes an instance (soft delete).
    pub fn delete_instance(
        &self,
        instance_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<u64, CoreError> {
        let instance_lock =
            self.instances
                .get(instance_id)
                .ok_or_else(|| CoreError::InstanceNotFound {
                    instance_id: instance_id.to_string(),
                })?;

        let mut instance = instance_lock.write();

        if instance.is_deleted() {
            // Already deleted, idempotent success
            return Ok(instance.last_wal_offset);
        }

        // Write to WAL
        let entry = WalEntry::DeleteInstance {
            instance_id: instance_id.to_string(),
            idempotency_key: idempotency_key.map(|s| s.to_string()),
        };
        let (_, offset) = self.wal.append(&entry)?;

        // Update instance
        instance.soft_delete(offset.as_u64());

        Ok(offset.as_u64())
    }

    // =========================================================================
    // Instance Enumeration (for snapshots/compaction)
    // =========================================================================

    /// Returns all instance IDs.
    pub fn list_instance_ids(&self) -> Vec<String> {
        self.instances.iter().map(|r| r.key().clone()).collect()
    }

    /// Returns clones of all non-deleted instances.
    pub fn get_all_instances(&self) -> Vec<Instance> {
        self.instances
            .iter()
            .map(|r| r.value().read().clone())
            .filter(|i| !i.is_deleted())
            .collect()
    }

    /// Returns the number of instances.
    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    // =========================================================================
    // WAL Access
    // =========================================================================

    /// Returns a reference to the WAL.
    pub fn wal(&self) -> &Arc<Wal> {
        &self.wal
    }

    /// Syncs the WAL to disk.
    pub fn sync(&self) -> Result<(), CoreError> {
        self.wal.sync()?;
        Ok(())
    }
}

/// Merges payload into context.
fn merge_ctx(ctx: &Value, payload: &Value) -> Value {
    match (ctx, payload) {
        (Value::Object(ctx_map), Value::Object(payload_map)) => {
            let mut result = ctx_map.clone();
            for (k, v) in payload_map {
                result.insert(k.clone(), v.clone());
            }
            Value::Object(result)
        }
        _ => ctx.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstmdb_wal::FsyncPolicy;
    use serde_json::json;
    use tempfile::TempDir;

    fn test_engine() -> (TempDir, StateMachineEngine) {
        let dir = TempDir::new().unwrap();
        let config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        let engine = StateMachineEngine::new(config).unwrap();
        (dir, engine)
    }

    fn sample_definition() -> Value {
        json!({
            "states": ["created", "paid", "shipped"],
            "initial": "created",
            "transitions": [
                {"from": "created", "event": "PAY", "to": "paid"},
                {"from": "paid", "event": "SHIP", "to": "shipped", "guard": "ctx.items_ready"}
            ]
        })
    }

    #[test]
    fn test_put_and_get_machine() {
        let (_dir, engine) = test_engine();

        let (checksum, created) = engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        assert!(created);
        assert!(!checksum.is_empty());

        let def = engine.get_machine("order", 1).unwrap();
        assert_eq!(def.name, "order");
        assert_eq!(def.version, 1);
    }

    #[test]
    fn test_put_machine_idempotent() {
        let (_dir, engine) = test_engine();

        let (checksum1, created1) = engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        assert!(created1);

        let (checksum2, created2) = engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        assert!(!created2);
        assert_eq!(checksum1, checksum2);
    }

    #[test]
    fn test_create_and_get_instance() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();

        let (instance, _) = engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();

        assert_eq!(instance.id, "i-1");
        assert_eq!(instance.state, "created");

        let fetched = engine.get_instance("i-1").unwrap();
        assert_eq!(fetched.id, "i-1");
    }

    #[test]
    fn test_apply_event() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();

        let result = engine
            .apply_event("i-1", "PAY", json!({"amount": 100}), None, None, None, None)
            .unwrap();

        assert_eq!(result.from_state, "created");
        assert_eq!(result.to_state, "paid");
        assert!(result.applied);

        let instance = engine.get_instance("i-1").unwrap();
        assert_eq!(instance.state, "paid");
        assert_eq!(instance.ctx["amount"], 100);
    }

    #[test]
    fn test_guard_evaluation() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({"items_ready": false}), None)
            .unwrap();
        engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, None)
            .unwrap();

        // Guard should fail
        let result = engine.apply_event("i-1", "SHIP", json!({}), None, None, None, None);
        assert!(matches!(result, Err(CoreError::GuardFailed { .. })));

        // Set items_ready and try again
        engine
            .apply_event(
                "i-1",
                "SHIP",
                json!({"items_ready": true}),
                None,
                None,
                None,
                None,
            )
            .unwrap_err(); // Still fails because ctx wasn't updated

        // Create new instance with items_ready
        engine
            .create_instance("i-2", "order", 1, json!({"items_ready": true}), None)
            .unwrap();
        engine
            .apply_event("i-2", "PAY", json!({}), None, None, None, None)
            .unwrap();
        let result = engine
            .apply_event("i-2", "SHIP", json!({}), None, None, None, None)
            .unwrap();
        assert_eq!(result.to_state, "shipped");
    }

    #[test]
    fn test_invalid_transition() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();

        let result = engine.apply_event("i-1", "SHIP", json!({}), None, None, None, None);
        assert!(matches!(result, Err(CoreError::InvalidTransition { .. })));
    }

    #[test]
    fn test_state_conflict() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();

        let result = engine.apply_event(
            "i-1",
            "PAY",
            json!({}),
            Some("paid"), // Wrong expected state
            None,
            None,
            None,
        );
        assert!(matches!(result, Err(CoreError::StateConflict { .. })));
    }

    #[test]
    fn test_idempotency() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();

        let result1 = engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("key-1"))
            .unwrap();

        let result2 = engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("key-1"))
            .unwrap();

        assert_eq!(result1.wal_offset, result2.wal_offset);
        assert_eq!(result1.to_state, result2.to_state);
    }
}
