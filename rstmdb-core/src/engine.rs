//! State machine engine - coordinates definitions, instances, and WAL.

use crate::definition::{MachineDefinition, State};
use crate::error::CoreError;
use crate::guard::GuardEvaluator;
use crate::instance::{IdempotencyRecord, Instance, InstanceSnapshot};
use dashmap::DashMap;
use parking_lot::RwLock;
use rstmdb_wal::{Wal, WalConfig, WalEntry};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
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

/// Cached `APPLY_EVENT` results of one instance, keyed by idempotency key.
///
/// `order` holds the keys in insertion order, which is WAL order on every
/// path (live writes, replay, snapshot records in the order they were
/// written), so its front is always the oldest key.
#[derive(Debug, Default)]
struct IdempotencyScope {
    results: HashMap<String, ApplyResult>,
    order: VecDeque<String>,
}

impl IdempotencyScope {
    fn get(&self, key: &str) -> Option<&ApplyResult> {
        self.results.get(key)
    }

    /// Inserts a result, then evicts the oldest keys beyond `max`
    /// (0 = unlimited). Re-inserting a present key replaces its result and
    /// moves it to the newest position.
    fn insert(&mut self, key: String, result: ApplyResult, max: usize) {
        if self.results.insert(key.clone(), result).is_some() {
            self.order.retain(|k| k != &key);
        }
        self.order.push_back(key);
        self.trim(max);
    }

    /// Keeps only the `max` newest keys (0 = unlimited).
    fn trim(&mut self, max: usize) {
        if max == 0 {
            return;
        }
        while self.order.len() > max {
            if let Some(oldest) = self.order.pop_front() {
                self.results.remove(&oldest);
            }
        }
    }
}

/// A persisted machine definition, used to recover definitions whose
/// PUT_MACHINE WAL entries were removed by compaction.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineRecord {
    pub machine: String,
    pub version: u32,
    pub definition: Value,
}

/// Durable state loaded from outside the WAL (snapshot store) at startup.
///
/// Recovery restores machines and instance snapshots first, then replays the
/// remaining WAL, skipping instance entries already covered by a snapshot
/// (`entry offset <= snapshot.wal_offset`).
/// Engine settings that recovery must already honour.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EngineOptions {
    /// Maximum idempotency keys kept per instance (0 = unlimited).
    pub max_idempotency_keys: usize,
}

#[derive(Debug, Clone, Default)]
pub struct RecoveryState {
    pub machines: Vec<MachineRecord>,
    pub snapshots: Vec<InstanceSnapshot>,
    /// WAL head when `machines` was written (0 for legacy files). Machines
    /// written before the last FLUSH_ALL marker are stale and ignored.
    pub machines_wal_offset: u64,
}

/// Outcome of [`StateMachineEngine::flush_all`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlushAllResult {
    pub instances_removed: usize,
    pub machines_removed: usize,
    /// Offset of the FlushAll marker in the WAL.
    pub wal_offset: u64,
}

/// The state machine engine.
pub struct StateMachineEngine {
    /// Machine definitions indexed by (name, version).
    definitions: DashMap<(String, u32), Arc<MachineDefinition>>,

    /// Instances indexed by ID.
    instances: DashMap<String, RwLock<Instance>>,

    /// Idempotency cache: instance_id -> (idempotency_key -> result).
    ///
    /// Rebuilt on startup from snapshots and WAL `idempotency_key`s, and
    /// persisted in instance snapshots, so it survives restarts and compaction.
    idempotency_cache: DashMap<String, IdempotencyScope>,

    /// Maximum idempotency keys kept per instance (0 = unlimited). The oldest
    /// key is evicted when a new one would exceed it.
    max_idempotency_keys: usize,

    /// Write-ahead log.
    wal: Arc<Wal>,

    /// Mutations hold it shared from WAL append to memory update; FLUSH_ALL
    /// holds it exclusively, so no write straddles the FlushAll marker.
    write_gate: RwLock<()>,

    /// Whether to allow re-creating deleted instances with the same ID.
    allow_instance_recreate: bool,

    /// Offset of the last FlushAll marker found in the WAL at recovery.
    last_flush_offset: Option<u64>,
}

impl StateMachineEngine {
    /// Creates a new engine with the given WAL configuration.
    /// Replays WAL entries to restore state.
    ///
    /// # Examples
    ///
    /// ```
    /// use rstmdb_core::StateMachineEngine;
    /// use rstmdb_wal::{WalConfig, FsyncPolicy};
    ///
    /// let dir = tempfile::TempDir::new().unwrap();
    /// let config = WalConfig::new(dir.path())
    ///     .with_fsync_policy(FsyncPolicy::Never);
    /// let engine = StateMachineEngine::new(config).unwrap();
    /// assert_eq!(engine.instance_count(), 0);
    /// ```
    pub fn new(wal_config: WalConfig) -> Result<Self, CoreError> {
        Self::new_with_recovery(wal_config, RecoveryState::default())
    }

    /// Opens the WAL and recovers state from `recovery` (snapshots and
    /// persisted machine definitions) plus the remaining WAL entries.
    pub fn new_with_recovery(
        wal_config: WalConfig,
        recovery: RecoveryState,
    ) -> Result<Self, CoreError> {
        Self::new_with_options(wal_config, recovery, EngineOptions::default())
    }

    /// Like [`Self::new_with_recovery`], with `options` applied during
    /// recovery (so the idempotency key limit also bounds startup memory).
    pub fn new_with_options(
        wal_config: WalConfig,
        recovery: RecoveryState,
        options: EngineOptions,
    ) -> Result<Self, CoreError> {
        Self::recover_with_options(Arc::new(Wal::open(wal_config)?), recovery, options)
    }

    /// Creates an engine over an existing WAL and recovers state.
    pub fn recover(wal: Arc<Wal>, recovery: RecoveryState) -> Result<Self, CoreError> {
        Self::recover_with_options(wal, recovery, EngineOptions::default())
    }

    /// Creates an engine over an existing WAL and recovers state, applying
    /// `options` while replaying.
    pub fn recover_with_options(
        wal: Arc<Wal>,
        recovery: RecoveryState,
        options: EngineOptions,
    ) -> Result<Self, CoreError> {
        let mut engine = Self {
            definitions: DashMap::new(),
            instances: DashMap::new(),
            idempotency_cache: DashMap::new(),
            wal,
            write_gate: RwLock::new(()),
            allow_instance_recreate: true,
            max_idempotency_keys: options.max_idempotency_keys,
            last_flush_offset: None,
        };

        engine.last_flush_offset = engine.restore(recovery)?;

        Ok(engine)
    }

    /// Sets whether re-creating deleted instances is allowed.
    pub fn with_allow_instance_recreate(mut self, allow: bool) -> Self {
        self.allow_instance_recreate = allow;
        self
    }

    /// Limits cached idempotency keys per instance to the `max` newest
    /// (0 = unlimited) and trims what recovery already loaded. Trimming to
    /// the newest keys gives the same set as evicting one by one during
    /// replay, because eviction always drops the oldest key.
    pub fn with_max_idempotency_keys(mut self, max: usize) -> Self {
        self.max_idempotency_keys = max;
        for mut scope in self.idempotency_cache.iter_mut() {
            scope.trim(max);
        }
        self
    }

    /// Maximum idempotency keys kept per instance (0 = unlimited).
    pub fn max_idempotency_keys(&self) -> usize {
        self.max_idempotency_keys
    }

    /// Creates a new engine with an existing WAL.
    /// Replays WAL entries to restore state.
    pub fn with_wal(wal: Arc<Wal>) -> Result<Self, CoreError> {
        Self::recover(wal, RecoveryState::default())
    }

    /// Restores persisted machines and snapshots, then replays the WAL.
    ///
    /// Everything before the last FlushAll marker (F) is gone: machines and
    /// snapshots persisted at or before F are ignored and only entries after
    /// F are replayed. Returns F.
    fn restore(&self, recovery: RecoveryState) -> Result<Option<u64>, CoreError> {
        use rstmdb_wal::WalOffset;

        let entries = self.wal.read_from(WalOffset::from_u64(0), None)?;
        let flush_offset = entries
            .iter()
            .rev()
            .find(|(_, _, e)| matches!(e, WalEntry::FlushAll { .. }))
            .map(|(_, offset, _)| offset.as_u64());
        // WAL head positions are end offsets, so a head captured just before
        // the flush equals F: anything at or before F predates the flush.
        let before_flush = |offset: u64| flush_offset.is_some_and(|f| offset <= f);

        let machines = if before_flush(recovery.machines_wal_offset) {
            Vec::new()
        } else {
            recovery.machines
        };
        let machine_count = machines.len();
        for record in machines {
            let key = (record.machine.clone(), record.version);
            if !self.definitions.contains_key(&key) {
                let def = MachineDefinition::from_json(
                    &record.machine,
                    record.version,
                    &record.definition,
                )?;
                self.definitions.insert(key, Arc::new(def));
            }
        }

        let (snapshots, stale): (Vec<_>, Vec<_>) = recovery
            .snapshots
            .into_iter()
            .partition(|s| !before_flush(s.wal_offset));
        if !stale.is_empty() {
            tracing::info!(
                "Ignoring {} snapshots older than FLUSH_ALL at offset {}",
                stale.len(),
                flush_offset.unwrap_or(0)
            );
        }

        // Instance id -> WAL offset already reflected by its snapshot.
        let mut covered: HashMap<String, u64> = HashMap::new();
        let snapshot_count = snapshots.len();
        for snapshot in snapshots {
            covered.insert(snapshot.instance_id.clone(), snapshot.wal_offset);
            if !snapshot.idempotency.is_empty() {
                let mut scope = IdempotencyScope::default();
                for r in &snapshot.idempotency {
                    scope.insert(
                        r.key.clone(),
                        ApplyResult {
                            from_state: r.from_state.clone(),
                            to_state: r.to_state.clone(),
                            ctx: r.ctx.clone(),
                            wal_offset: r.wal_offset,
                            sequence: r.sequence,
                            applied: true,
                        },
                        self.max_idempotency_keys,
                    );
                }
                self.idempotency_cache
                    .insert(snapshot.instance_id.clone(), scope);
            }
            let mut instance = snapshot.to_instance();
            instance.ctx = normalize_ctx(instance.ctx).unwrap_or_else(empty_ctx);
            self.instances
                .insert(snapshot.instance_id.clone(), RwLock::new(instance));
        }

        let entry_count = entries.len();
        let mut skipped = 0usize;

        for (seq, offset, entry) in entries {
            let offset = offset.as_u64();
            if flush_offset.is_some_and(|f| offset <= f) {
                continue;
            }
            if let Some(id) = entry_instance_id(&entry) {
                if covered
                    .get(id)
                    .is_some_and(|&snap_offset| offset <= snap_offset)
                {
                    skipped += 1;
                    continue;
                }
            }
            self.replay_entry_at(seq, offset, entry)?;
        }

        if entry_count > 0 || snapshot_count > 0 || machine_count > 0 {
            tracing::info!(
                "Recovery complete: {} persisted machines, {} snapshots, {} WAL entries ({} covered by snapshots); {} machines, {} instances",
                machine_count,
                snapshot_count,
                entry_count,
                skipped,
                self.definitions.len(),
                self.instances.len()
            );
        }

        Ok(flush_offset)
    }

    /// Replays a single WAL entry to restore in-memory state.
    /// This is used during WAL recovery and replication.
    pub fn replay_entry(&self, offset: u64, entry: WalEntry) -> Result<(), CoreError> {
        self.replay_entry_at(0, offset, entry)
    }

    fn replay_entry_at(
        &self,
        sequence: u64,
        offset: u64,
        entry: WalEntry,
    ) -> Result<(), CoreError> {
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
                idempotency_key,
            } => {
                let should_create = match self.instances.get(&instance_id) {
                    None => true,
                    Some(existing) => existing.read().is_deleted(),
                };
                if should_create {
                    // Older WALs may hold `null` contexts; heal them on replay.
                    let initial_ctx = normalize_ctx(initial_ctx).unwrap_or_else(empty_ctx);
                    let mut instance = Instance::new(
                        instance_id.clone(),
                        machine,
                        version,
                        initial_state,
                        initial_ctx,
                        offset,
                    );
                    instance.create_idempotency_key = idempotency_key;
                    // A (re)created instance starts with a fresh idempotency scope.
                    self.idempotency_cache.remove(&instance_id);
                    self.instances
                        .insert(instance_id, parking_lot::RwLock::new(instance));
                }
            }

            WalEntry::ApplyEvent {
                instance_id,
                from_state,
                to_state,
                ctx,
                event_id,
                idempotency_key,
                ..
            } => {
                if let Some(instance_lock) = self.instances.get(&instance_id) {
                    let mut instance = instance_lock.write();
                    if let Some(key) = idempotency_key {
                        self.idempotency_cache
                            .entry(instance_id.clone())
                            .or_default()
                            .insert(
                                key,
                                ApplyResult {
                                    from_state,
                                    to_state: to_state.clone(),
                                    ctx: ctx.clone(),
                                    wal_offset: offset,
                                    sequence,
                                    applied: true,
                                },
                                self.max_idempotency_keys,
                            );
                    }
                    instance.state = to_state;
                    instance.ctx = normalize_ctx(ctx).unwrap_or_else(empty_ctx);
                    instance.last_wal_offset = offset;
                    // Mirror apply_transition: an event without an ID clears it.
                    instance.last_event_id = event_id;
                }
            }

            WalEntry::DeleteInstance { instance_id, .. } => {
                if let Some(instance_lock) = self.instances.get(&instance_id) {
                    let mut instance = instance_lock.write();
                    instance.instance_state = crate::instance::InstanceState::Deleted;
                    instance.last_wal_offset = offset;
                }
            }

            WalEntry::FlushAll { .. } => self.clear_state(),

            WalEntry::Snapshot { .. } | WalEntry::Checkpoint { .. } => {
                // These don't affect in-memory state
            }
        }

        Ok(())
    }

    /// Applies a replicated WAL entry from a primary server.
    /// Appends the entry to the local WAL, then replays it using the primary's
    /// offset so the replica's in-memory `last_wal_offset` matches the primary's.
    ///
    /// **Restart caveat:** after a replica restart, `replay_wal()` uses local
    /// offsets during recovery, so in-memory offsets revert to local values
    /// until the replica catches up from the primary again. For cross-node
    /// comparisons, use the WAL `sequence` (which matches across nodes).
    ///
    /// Returns the replica's local `(sequence, offset)` after appending.
    pub fn apply_replicated_entry(
        &self,
        primary_offset: u64,
        entry: WalEntry,
    ) -> Result<(u64, u64), CoreError> {
        let _write = self.write_gate.read();
        let (sequence, local_offset) = self.wal.append(&entry)?;
        // Use the primary's offset in memory so clients see consistent offsets
        // when reading from any node that's fully caught up. The local
        // sequence matches the primary's, so cached results keep it.
        self.replay_entry_at(sequence, primary_offset, entry)?;
        Ok((sequence, local_offset.as_u64()))
    }

    // =========================================================================
    // Machine Definition Management
    // =========================================================================

    /// Registers a machine definition.
    ///
    /// Returns `(checksum, created)` where `created` is `true` if this is a new definition.
    ///
    /// # Examples
    ///
    /// ```
    /// # use rstmdb_core::StateMachineEngine;
    /// # use rstmdb_wal::{WalConfig, FsyncPolicy};
    /// # use serde_json::json;
    /// # let dir = tempfile::TempDir::new().unwrap();
    /// # let engine = StateMachineEngine::new(
    /// #     WalConfig::new(dir.path()).with_fsync_policy(FsyncPolicy::Never)
    /// # ).unwrap();
    /// let (checksum, created) = engine.put_machine("order", 1, &json!({
    ///     "states": ["created", "paid"],
    ///     "initial": "created",
    ///     "transitions": [
    ///         {"from": "created", "event": "PAY", "to": "paid"}
    ///     ]
    /// })).unwrap();
    ///
    /// assert!(created);
    /// assert!(!checksum.is_empty());
    /// ```
    pub fn put_machine(
        &self,
        name: &str,
        version: u32,
        definition_json: &Value,
    ) -> Result<(String, bool), CoreError> {
        let _write = self.write_gate.read();
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

    /// Gets all versions for a specific machine.
    pub fn get_machine_versions(&self, name: &str) -> Vec<u32> {
        let mut versions: Vec<u32> = self
            .definitions
            .iter()
            .filter_map(|entry| {
                let (n, v) = entry.key();
                if n == name {
                    Some(*v)
                } else {
                    None
                }
            })
            .collect();
        versions.sort();
        versions
    }

    // =========================================================================
    // Instance Management
    // =========================================================================

    /// Creates a new instance of a registered state machine.
    ///
    /// # Examples
    ///
    /// ```
    /// # use rstmdb_core::StateMachineEngine;
    /// # use rstmdb_wal::{WalConfig, FsyncPolicy};
    /// # use serde_json::json;
    /// # let dir = tempfile::TempDir::new().unwrap();
    /// # let engine = StateMachineEngine::new(
    /// #     WalConfig::new(dir.path()).with_fsync_policy(FsyncPolicy::Never)
    /// # ).unwrap();
    /// # engine.put_machine("order", 1, &json!({
    /// #     "states": ["created", "paid"],
    /// #     "initial": "created",
    /// #     "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
    /// # })).unwrap();
    /// let (instance, wal_offset) = engine.create_instance(
    ///     "order-001", "order", 1,
    ///     json!({"customer": "alice"}),
    ///     None,
    /// ).unwrap();
    ///
    /// assert_eq!(instance.state, "created");
    /// assert_eq!(instance.id, "order-001");
    /// ```
    pub fn create_instance(
        &self,
        instance_id: &str,
        machine: &str,
        version: u32,
        initial_ctx: Value,
        idempotency_key: Option<&str>,
    ) -> Result<(Instance, u64), CoreError> {
        let _write = self.write_gate.read();
        // The context is always a JSON object; an omitted/null initial_ctx
        // means `{}` (anything else would make later payload merges no-ops).
        let initial_ctx = normalize_ctx(initial_ctx).ok_or_else(|| CoreError::InvalidContext {
            reason: "initial_ctx must be a JSON object".to_string(),
        })?;

        // Idempotent retry: the live instance was created by a request with
        // the same key (and the same machine/version).
        if let Some(key) = idempotency_key {
            if let Some(instance_lock) = self.instances.get(instance_id) {
                let instance = instance_lock.read();
                if !instance.is_deleted()
                    && instance.create_idempotency_key.as_deref() == Some(key)
                    && instance.machine == machine
                    && instance.version == version
                {
                    return Ok((instance.clone(), instance.last_wal_offset));
                }
            }
        }

        // Check if instance already exists
        if let Some(existing) = self.instances.get(instance_id) {
            let is_deleted = existing.read().is_deleted();
            drop(existing); // Release the map guard before mutating

            if is_deleted && self.allow_instance_recreate {
                self.instances.remove(instance_id);
            } else {
                return Err(CoreError::InstanceExists {
                    instance_id: instance_id.to_string(),
                });
            }
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
        let mut instance = Instance::new(
            instance_id,
            machine,
            version,
            definition.initial.as_str(),
            initial_ctx,
            offset.as_u64(),
        );
        instance.create_idempotency_key = idempotency_key.map(|s| s.to_string());

        // A (re)created instance starts with a fresh idempotency scope.
        self.idempotency_cache.remove(instance_id);

        // Store
        self.instances
            .insert(instance_id.to_string(), RwLock::new(instance.clone()));

        Ok((instance, sequence))
    }

    /// Gets an instance by ID. Returns `InstanceNotFound` for deleted instances.
    pub fn get_instance(&self, instance_id: &str) -> Result<Instance, CoreError> {
        let instance = self
            .instances
            .get(instance_id)
            .map(|r| r.read().clone())
            .ok_or_else(|| CoreError::InstanceNotFound {
                instance_id: instance_id.to_string(),
            })?;

        if instance.is_deleted() {
            return Err(CoreError::InstanceNotFound {
                instance_id: instance_id.to_string(),
            });
        }

        Ok(instance)
    }

    /// Applies an event to an instance, triggering a state transition.
    ///
    /// # Examples
    ///
    /// ```
    /// # use rstmdb_core::StateMachineEngine;
    /// # use rstmdb_wal::{WalConfig, FsyncPolicy};
    /// # use serde_json::json;
    /// # let dir = tempfile::TempDir::new().unwrap();
    /// # let engine = StateMachineEngine::new(
    /// #     WalConfig::new(dir.path()).with_fsync_policy(FsyncPolicy::Never)
    /// # ).unwrap();
    /// # engine.put_machine("order", 1, &json!({
    /// #     "states": ["created", "paid"],
    /// #     "initial": "created",
    /// #     "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
    /// # })).unwrap();
    /// # engine.create_instance("order-001", "order", 1, json!({}), None).unwrap();
    /// let result = engine.apply_event(
    ///     "order-001", "PAY",
    ///     json!({"amount": 99.99}),
    ///     None, None, None, None,
    /// ).unwrap();
    ///
    /// assert_eq!(result.from_state, "created");
    /// assert_eq!(result.to_state, "paid");
    /// assert!(result.applied);
    /// ```
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
        let _write = self.write_gate.read();
        // Get instance (with write lock for the duration)
        let instance_lock =
            self.instances
                .get(instance_id)
                .ok_or_else(|| CoreError::InstanceNotFound {
                    instance_id: instance_id.to_string(),
                })?;

        let mut instance = instance_lock.write();

        // Check idempotency cache under the instance lock, so two concurrent
        // requests with the same key cannot both apply. A replay reports
        // `applied: false` (the event was not applied again).
        if let Some(key) = idempotency_key {
            if let Some(cached) = self
                .idempotency_cache
                .get(instance_id)
                .and_then(|m| m.get(key).cloned())
            {
                return Ok(ApplyResult {
                    applied: false,
                    ..cached
                });
            }
        }

        // Reject events on deleted instances
        if instance.is_deleted() {
            return Err(CoreError::InstanceNotFound {
                instance_id: instance_id.to_string(),
            });
        }

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

        // Strict guard mode: a missing field fails closed before evaluation, so it can never
        // make a negated guard or an inequality pass.
        if definition.strict_guards() {
            if let Some(path) = guard.and_then(|g| g.first_missing_path(&instance.ctx)) {
                return Err(CoreError::GuardUnknown {
                    path: format!("ctx.{}", path),
                    reason: format!(
                        "strict_guards: transition '{}' -> '{}' on event '{}'",
                        instance.state,
                        to_state.as_str(),
                        event
                    ),
                });
            }
        }

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
            self.idempotency_cache
                .entry(instance_id.to_string())
                .or_default()
                .insert(key.to_string(), result.clone(), self.max_idempotency_keys);
        }

        Ok(result)
    }

    /// Deletes an instance (soft delete).
    ///
    /// After deletion, `get_instance` and `apply_event` return `InstanceNotFound`.
    ///
    /// # Examples
    ///
    /// ```
    /// # use rstmdb_core::StateMachineEngine;
    /// # use rstmdb_wal::{WalConfig, FsyncPolicy};
    /// # use serde_json::json;
    /// # let dir = tempfile::TempDir::new().unwrap();
    /// # let engine = StateMachineEngine::new(
    /// #     WalConfig::new(dir.path()).with_fsync_policy(FsyncPolicy::Never)
    /// # ).unwrap();
    /// # engine.put_machine("order", 1, &json!({
    /// #     "states": ["created"], "initial": "created", "transitions": []
    /// # })).unwrap();
    /// # engine.create_instance("i1", "order", 1, json!({}), None).unwrap();
    /// engine.delete_instance("i1", None).unwrap();
    /// assert!(engine.get_instance("i1").is_err());
    /// ```
    pub fn delete_instance(
        &self,
        instance_id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<u64, CoreError> {
        let _write = self.write_gate.read();
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
    // Administrative
    // =========================================================================

    /// Drops every machine, instance and idempotency record from memory.
    fn clear_state(&self) {
        self.instances.clear();
        self.definitions.clear();
        self.idempotency_cache.clear();
    }

    /// Clears all instances, machine definitions, and idempotency records.
    ///
    /// Durable: a FlushAll marker is appended and fsynced (regardless of the
    /// fsync policy), so recovery never brings the flushed data back. If the
    /// append fails nothing is cleared; once it succeeds memory is cleared
    /// even if the fsync then fails (the error is still returned).
    ///
    /// # Examples
    ///
    /// ```
    /// # use rstmdb_core::StateMachineEngine;
    /// # use rstmdb_wal::{WalConfig, FsyncPolicy};
    /// # use serde_json::json;
    /// # let dir = tempfile::TempDir::new().unwrap();
    /// # let engine = StateMachineEngine::new(
    /// #     WalConfig::new(dir.path()).with_fsync_policy(FsyncPolicy::Never)
    /// # ).unwrap();
    /// # engine.put_machine("order", 1, &json!({
    /// #     "states": ["created"], "initial": "created", "transitions": []
    /// # })).unwrap();
    /// # engine.create_instance("i1", "order", 1, json!({}), None).unwrap();
    /// let flushed = engine.flush_all().unwrap();
    /// assert_eq!(flushed.instances_removed, 1);
    /// assert_eq!(flushed.machines_removed, 1);
    /// assert_eq!(engine.instance_count(), 0);
    /// ```
    pub fn flush_all(&self) -> Result<FlushAllResult, CoreError> {
        let _write = self.write_gate.write();

        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let (_, offset) = self.wal.append(&WalEntry::FlushAll { timestamp })?;

        // The marker is in the WAL now and recovery will honour it, so memory
        // follows it even if the fsync below fails.
        let result = FlushAllResult {
            instances_removed: self.instances.len(),
            machines_removed: self.definitions.len(),
            wal_offset: offset.as_u64(),
        };
        self.clear_state();

        self.wal.sync()?;
        Ok(result)
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
    /// All instances, including soft-deleted ones (as tombstones), each with
    /// its cached idempotency results, read consistently under the instance
    /// lock. Used to build snapshots.
    pub fn instances_for_snapshot(&self) -> Vec<(Instance, Vec<IdempotencyRecord>)> {
        self.instances
            .iter()
            .map(|r| {
                let instance = r.value().read();
                let records = self.idempotency_records(&instance.id);
                (instance.clone(), records)
            })
            .collect()
    }

    /// Cached idempotency results for one instance, oldest first (the order
    /// eviction uses, which is WAL-offset order).
    pub fn idempotency_records(&self, instance_id: &str) -> Vec<IdempotencyRecord> {
        self.idempotency_cache
            .get(instance_id)
            .map(|scope| {
                scope
                    .order
                    .iter()
                    .filter_map(|key| {
                        scope.get(key).map(|r| IdempotencyRecord {
                            key: key.clone(),
                            from_state: r.from_state.clone(),
                            to_state: r.to_state.clone(),
                            ctx: r.ctx.clone(),
                            wal_offset: r.wal_offset,
                            sequence: r.sequence,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// All machine definitions, for persisting alongside snapshots.
    pub fn machine_records(&self) -> Result<Vec<MachineRecord>, CoreError> {
        let mut records = Vec::with_capacity(self.definitions.len());
        for entry in self.definitions.iter() {
            let (machine, version) = entry.key();
            records.push(MachineRecord {
                machine: machine.clone(),
                version: *version,
                definition: serde_json::to_value(&entry.value().raw)?,
            });
        }
        records.sort_by(|a, b| a.machine.cmp(&b.machine).then(a.version.cmp(&b.version)));
        Ok(records)
    }

    pub fn instance_count(&self) -> usize {
        self.instances.len()
    }

    // =========================================================================
    // WAL Access
    // =========================================================================

    /// Offset of the last FLUSH_ALL marker seen during recovery. Snapshot
    /// store state older than it is stale and should be discarded.
    pub fn last_flush_offset(&self) -> Option<u64> {
        self.last_flush_offset
    }

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
fn empty_ctx() -> Value {
    Value::Object(serde_json::Map::new())
}

/// `null` -> `{}`, objects unchanged, anything else -> `None` (invalid).
fn normalize_ctx(ctx: Value) -> Option<Value> {
    match ctx {
        Value::Null => Some(empty_ctx()),
        Value::Object(_) => Some(ctx),
        _ => None,
    }
}

fn entry_instance_id(entry: &WalEntry) -> Option<&str> {
    match entry {
        WalEntry::CreateInstance { instance_id, .. }
        | WalEntry::ApplyEvent { instance_id, .. }
        | WalEntry::DeleteInstance { instance_id, .. } => Some(instance_id),
        _ => None,
    }
}

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

    fn reopen_with(dir: &TempDir, recovery: RecoveryState) -> StateMachineEngine {
        let config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        StateMachineEngine::new_with_recovery(config, recovery).unwrap()
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

    fn gate_definition(strict: bool) -> Value {
        let mut def = json!({
            "states": ["open", "done"],
            "initial": "open",
            "transitions": [
                {"from": "open", "event": "CLOSE", "to": "done", "guard": "!ctx.blocked"},
                {"from": "open", "event": "ACCEPT", "to": "done", "guard": "ctx.status != \"rejected\""}
            ]
        });
        if strict {
            def["strict_guards"] = json!(true);
        }
        def
    }

    #[test]
    fn test_strict_guards_reject_missing_negated_field() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("gate", 1, &gate_definition(true))
            .unwrap();
        engine
            .create_instance("g-1", "gate", 1, json!({"other": 1}), None)
            .unwrap();
        let before = engine.get_instance("g-1").unwrap();
        let wal_seq_before = engine.wal().next_sequence();

        let err = engine
            .apply_event("g-1", "CLOSE", json!({}), None, None, None, None)
            .unwrap_err();
        match &err {
            CoreError::GuardUnknown { path, .. } => assert_eq!(path, "ctx.blocked"),
            other => panic!("expected GuardUnknown, got {:?}", other),
        }
        assert_eq!(err.error_code(), "GUARD_UNKNOWN");
        assert!(err.to_string().contains("ctx.blocked"));

        let after = engine.get_instance("g-1").unwrap();
        assert_eq!(after.state, "open");
        assert_eq!(after.ctx, before.ctx);
        assert_eq!(after.last_wal_offset, before.last_wal_offset);
        assert_eq!(engine.wal().next_sequence(), wal_seq_before);
    }

    #[test]
    fn test_strict_guards_reject_missing_field_in_inequality() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("gate", 1, &gate_definition(true))
            .unwrap();
        engine
            .create_instance("g-1", "gate", 1, json!({}), None)
            .unwrap();

        let err = engine
            .apply_event("g-1", "ACCEPT", json!({}), None, None, None, None)
            .unwrap_err();
        match &err {
            CoreError::GuardUnknown { path, .. } => assert_eq!(path, "ctx.status"),
            other => panic!("expected GuardUnknown, got {:?}", other),
        }
        assert_eq!(engine.get_instance("g-1").unwrap().state, "open");
    }

    #[test]
    fn test_strict_guards_evaluate_present_fields_normally() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("gate", 1, &gate_definition(true))
            .unwrap();

        // Present and true: the negated guard fails the ordinary way.
        engine
            .create_instance("g-1", "gate", 1, json!({"blocked": true}), None)
            .unwrap();
        let result = engine.apply_event("g-1", "CLOSE", json!({}), None, None, None, None);
        assert!(matches!(result, Err(CoreError::GuardFailed { .. })));

        // Present and false: passes.
        engine
            .create_instance("g-2", "gate", 1, json!({"blocked": false}), None)
            .unwrap();
        let result = engine
            .apply_event("g-2", "CLOSE", json!({}), None, None, None, None)
            .unwrap();
        assert_eq!(result.to_state, "done");

        // Present with an explicit null is a present field, not a missing one.
        engine
            .create_instance("g-3", "gate", 1, json!({"blocked": null}), None)
            .unwrap();
        let result = engine
            .apply_event("g-3", "CLOSE", json!({}), None, None, None, None)
            .unwrap();
        assert_eq!(result.to_state, "done");
    }

    #[test]
    fn test_lenient_guards_keep_missing_field_as_null() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("gate", 1, &gate_definition(false))
            .unwrap();
        engine
            .create_instance("g-1", "gate", 1, json!({}), None)
            .unwrap();
        let result = engine
            .apply_event("g-1", "CLOSE", json!({}), None, None, None, None)
            .unwrap();
        assert_eq!(result.to_state, "done");
    }

    #[test]
    fn test_strict_guards_survive_wal_replay() {
        let dir = TempDir::new().unwrap();
        let config = || {
            WalConfig::new(dir.path())
                .with_segment_size(4096)
                .with_fsync_policy(FsyncPolicy::EveryWrite)
        };
        {
            let engine = StateMachineEngine::new(config()).unwrap();
            engine
                .put_machine("gate", 1, &gate_definition(true))
                .unwrap();
            engine
                .create_instance("g-1", "gate", 1, json!({}), None)
                .unwrap();
        }

        let engine = StateMachineEngine::new(config()).unwrap();
        assert!(engine.get_machine("gate", 1).unwrap().strict_guards());
        let result = engine.apply_event("g-1", "CLOSE", json!({}), None, None, None, None);
        assert!(matches!(result, Err(CoreError::GuardUnknown { .. })));
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

    // =========================================================================
    // Context Normalization Tests
    // =========================================================================

    #[test]
    fn test_null_initial_ctx_becomes_empty_object() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        let (instance, _) = engine
            .create_instance("i-1", "order", 1, Value::Null, None)
            .unwrap();
        assert_eq!(instance.ctx, json!({}));

        // Payloads merge (previously a null ctx silently dropped them).
        engine
            .apply_event(
                "i-1",
                "PAY",
                json!({"items_ready": true}),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        assert_eq!(
            engine.get_instance("i-1").unwrap().ctx,
            json!({"items_ready": true})
        );
        drop(engine);

        let engine = reopen(&dir);
        assert_eq!(
            engine.get_instance("i-1").unwrap().ctx,
            json!({"items_ready": true})
        );
    }

    #[test]
    fn test_non_object_initial_ctx_rejected() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        for bad in [json!([1, 2]), json!("x"), json!(3)] {
            let err = engine
                .create_instance("i-1", "order", 1, bad, None)
                .unwrap_err();
            assert_eq!(err.error_code(), "BAD_REQUEST");
        }
    }

    #[test]
    fn test_replay_heals_legacy_null_ctx() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        // Simulate a WAL written by an older server that stored `null`.
        engine
            .wal()
            .append(&WalEntry::CreateInstance {
                instance_id: "legacy".to_string(),
                machine: "order".to_string(),
                version: 1,
                initial_state: "created".to_string(),
                initial_ctx: Value::Null,
                idempotency_key: None,
            })
            .unwrap();
        drop(engine);

        let engine = reopen(&dir);
        assert_eq!(engine.get_instance("legacy").unwrap().ctx, json!({}));
        engine
            .apply_event("legacy", "PAY", json!({"a": 1}), None, None, None, None)
            .unwrap();
        assert_eq!(engine.get_instance("legacy").unwrap().ctx, json!({"a": 1}));
    }

    // =========================================================================
    // Durable Idempotency Tests
    // =========================================================================

    fn reopen(dir: &TempDir) -> StateMachineEngine {
        let config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        StateMachineEngine::new(config).unwrap()
    }

    #[test]
    fn test_idempotent_replay_reports_not_applied() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();

        let first = engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("k"))
            .unwrap();
        let replay = engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("k"))
            .unwrap();

        assert!(first.applied);
        assert!(!replay.applied);
        assert_eq!(first.wal_offset, replay.wal_offset);
        assert_eq!(engine.get_instance("i-1").unwrap().state, "paid");
    }

    #[test]
    fn test_apply_idempotency_survives_restart() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();
        let first = engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("pay-1"))
            .unwrap();
        drop(engine);

        let engine = reopen(&dir);
        let retry = engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("pay-1"))
            .unwrap();

        assert!(!retry.applied, "retry after restart must not re-apply");
        assert_eq!(retry.wal_offset, first.wal_offset);
        assert_eq!(retry.to_state, "paid");
        assert_eq!(engine.get_instance("i-1").unwrap().state, "paid");
    }

    #[test]
    fn test_create_idempotency() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        let (created, _) = engine
            .create_instance("i-1", "order", 1, json!({"a": 1}), Some("create-1"))
            .unwrap();

        // Same key: returns the existing instance.
        let (again, _) = engine
            .create_instance("i-1", "order", 1, json!({"a": 1}), Some("create-1"))
            .unwrap();
        assert_eq!(again.last_wal_offset, created.last_wal_offset);

        // Different key, or no key: still a conflict.
        assert!(matches!(
            engine.create_instance("i-1", "order", 1, json!({}), Some("other")),
            Err(CoreError::InstanceExists { .. })
        ));
        assert!(matches!(
            engine.create_instance("i-1", "order", 1, json!({}), None),
            Err(CoreError::InstanceExists { .. })
        ));
        drop(engine);

        // Survives restart.
        let engine = reopen(&dir);
        let (after, _) = engine
            .create_instance("i-1", "order", 1, json!({"a": 1}), Some("create-1"))
            .unwrap();
        assert_eq!(after.last_wal_offset, created.last_wal_offset);
    }

    #[test]
    fn test_recreated_instance_gets_fresh_idempotency_scope() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();
        engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("k"))
            .unwrap();
        engine.delete_instance("i-1", None).unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();

        let r = engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("k"))
            .unwrap();
        assert!(r.applied, "key from the deleted incarnation must not match");
        drop(engine);

        // Replay must reach the same conclusion.
        let engine = reopen(&dir);
        let r = engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("k"))
            .unwrap();
        assert!(!r.applied);
    }

    #[test]
    fn test_concurrent_same_key_applies_once() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();
        let engine = Arc::new(engine);

        let applied: usize = std::thread::scope(|s| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let engine = engine.clone();
                    s.spawn(move || {
                        engine
                            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("same"))
                            .map(|r| r.applied)
                            .unwrap_or(false)
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap() as usize)
                .sum()
        });

        assert_eq!(applied, 1);
        assert_eq!(engine.get_instance("i-1").unwrap().state, "paid");
    }

    #[test]
    fn test_recovery_from_snapshots_skips_covered_entries() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();
        engine
            .apply_event(
                "i-1",
                "PAY",
                json!({"items_ready": true}),
                None,
                None,
                Some("e-1"),
                Some("pay"),
            )
            .unwrap();

        // Snapshot at the current state, then advance past it.
        let (instance, records) = engine
            .instances_for_snapshot()
            .into_iter()
            .find(|(i, _)| i.id == "i-1")
            .unwrap();
        let snapshot = InstanceSnapshot::from_instance(&instance, "s").with_idempotency(records);
        let machines = engine.machine_records().unwrap();
        engine
            .apply_event("i-1", "SHIP", json!({}), None, None, None, Some("ship"))
            .unwrap();
        drop(engine);

        let config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        let engine = StateMachineEngine::new_with_recovery(
            config,
            RecoveryState {
                machines,
                snapshots: vec![snapshot],
                machines_wal_offset: 0,
            },
        )
        .unwrap();

        let i = engine.get_instance("i-1").unwrap();
        assert_eq!(i.state, "shipped");
        // SHIP carried no event_id, so (as in live apply) it cleared e-1.
        assert_eq!(i.last_event_id, None);
        for key in ["pay", "ship"] {
            let event = if key == "pay" { "PAY" } else { "SHIP" };
            let r = engine
                .apply_event("i-1", event, json!({}), None, None, None, Some(key))
                .unwrap();
            assert!(!r.applied, "{key} must be remembered");
        }
    }

    // =========================================================================
    // Delete + Recreate Tests
    // =========================================================================

    #[test]
    fn test_delete_instance() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-del", "order", 1, json!({}), None)
            .unwrap();

        let offset = engine.delete_instance("i-del", None).unwrap();
        assert!(offset > 0);

        // Should not be retrievable after delete
        let err = engine.get_instance("i-del").unwrap_err();
        assert!(matches!(err, CoreError::InstanceNotFound { .. }));
    }

    #[test]
    fn test_delete_instance_idempotent() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-del2", "order", 1, json!({}), None)
            .unwrap();

        // First delete succeeds
        engine.delete_instance("i-del2", None).unwrap();
        // Second delete is idempotent (returns existing offset)
        engine.delete_instance("i-del2", None).unwrap();
    }

    #[test]
    fn test_apply_event_on_deleted_instance_fails() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-del3", "order", 1, json!({}), None)
            .unwrap();

        engine.delete_instance("i-del3", None).unwrap();

        let err = engine
            .apply_event("i-del3", "PAY", json!({}), None, None, None, None)
            .unwrap_err();
        assert!(matches!(err, CoreError::InstanceNotFound { .. }));
    }

    #[test]
    fn test_deleted_instance_excluded_from_list() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-list1", "order", 1, json!({}), None)
            .unwrap();
        engine
            .create_instance("i-list2", "order", 1, json!({}), None)
            .unwrap();

        assert_eq!(engine.get_all_instances().len(), 2);

        engine.delete_instance("i-list1", None).unwrap();

        let instances = engine.get_all_instances();
        assert_eq!(instances.len(), 1);
        assert_eq!(instances[0].id, "i-list2");
    }

    #[test]
    fn test_recreate_deleted_instance_allowed() {
        let (_dir, engine) = test_engine();
        // allow_instance_recreate is true by default
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();

        // Create, use, and delete
        engine
            .create_instance("i-reuse", "order", 1, json!({"round": 1}), None)
            .unwrap();
        engine
            .apply_event("i-reuse", "PAY", json!({}), None, None, None, None)
            .unwrap();
        engine.delete_instance("i-reuse", None).unwrap();

        // Re-create with same ID — should succeed
        let (instance, _) = engine
            .create_instance("i-reuse", "order", 1, json!({"round": 2}), None)
            .unwrap();
        assert_eq!(instance.state, "created"); // Fresh instance, back to initial state
        assert_eq!(instance.ctx, json!({"round": 2}));

        // Should be fully functional
        let result = engine
            .apply_event("i-reuse", "PAY", json!({}), None, None, None, None)
            .unwrap();
        assert_eq!(result.to_state, "paid");
    }

    #[test]
    fn test_recreate_deleted_instance_disallowed() {
        let dir = TempDir::new().unwrap();
        let config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        let engine = StateMachineEngine::new(config)
            .unwrap()
            .with_allow_instance_recreate(false);

        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-noreuse", "order", 1, json!({}), None)
            .unwrap();
        engine.delete_instance("i-noreuse", None).unwrap();

        // Re-create should fail when allow_instance_recreate is false
        let err = engine
            .create_instance("i-noreuse", "order", 1, json!({}), None)
            .unwrap_err();
        assert!(matches!(err, CoreError::InstanceExists { .. }));
    }

    #[test]
    fn test_recreate_active_instance_always_fails() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-active", "order", 1, json!({}), None)
            .unwrap();

        // Creating again without deleting should always fail
        let err = engine
            .create_instance("i-active", "order", 1, json!({}), None)
            .unwrap_err();
        assert!(matches!(err, CoreError::InstanceExists { .. }));
    }

    // =========================================================================
    // Flush All Tests
    // =========================================================================

    #[test]
    fn test_flush_all() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .put_machine(
                "user",
                1,
                &json!({
                    "states": ["active"],
                    "initial": "active",
                    "transitions": []
                }),
            )
            .unwrap();
        engine
            .create_instance("i1", "order", 1, json!({}), None)
            .unwrap();
        engine
            .create_instance("i2", "order", 1, json!({}), None)
            .unwrap();

        let flushed = engine.flush_all().unwrap();
        assert_eq!(flushed.instances_removed, 2);
        assert_eq!(flushed.machines_removed, 2);

        // Everything should be gone
        assert!(engine.get_instance("i1").is_err());
        assert!(engine.get_instance("i2").is_err());
        assert!(engine.get_machine("order", 1).is_err());
        assert_eq!(engine.get_all_instances().len(), 0);
    }

    #[test]
    fn test_flush_all_then_recreate() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i1", "order", 1, json!({}), None)
            .unwrap();

        engine.flush_all().unwrap();

        // Can re-register machine and create instances after flush
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        let (instance, _) = engine
            .create_instance("i1", "order", 1, json!({"fresh": true}), None)
            .unwrap();
        assert_eq!(instance.state, "created");
        assert_eq!(instance.ctx, json!({"fresh": true}));
    }

    #[test]
    fn test_flush_all_empty_database() {
        let (_dir, engine) = test_engine();
        let flushed = engine.flush_all().unwrap();
        assert_eq!(flushed.instances_removed, 0);
        assert_eq!(flushed.machines_removed, 0);
    }

    #[test]
    fn test_flush_all_survives_restart() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();

        let flushed = engine.flush_all().unwrap();
        assert_eq!(flushed.instances_removed, 1);
        assert_eq!(flushed.machines_removed, 1);
        drop(engine);

        let engine = reopen_with(&dir, RecoveryState::default());
        assert_eq!(engine.instance_count(), 0);
        assert!(engine.get_machine("order", 1).is_err());
    }

    #[test]
    fn test_flush_all_empty_database_writes_marker() {
        let (dir, engine) = test_engine();
        let flushed = engine.flush_all().unwrap();
        assert_eq!(flushed.instances_removed, 0);
        assert_eq!(flushed.machines_removed, 0);
        let entries = engine
            .wal()
            .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
            .unwrap();
        assert!(matches!(
            entries.last(),
            Some((_, _, WalEntry::FlushAll { .. }))
        ));
        drop(engine);
        assert_eq!(
            reopen_with(&dir, RecoveryState::default()).instance_count(),
            0
        );
    }

    /// Writers racing FLUSH_ALL must never leave memory disagreeing with what
    /// the WAL replays to.
    #[test]
    fn test_flush_all_concurrent_writes_match_wal() {
        for round in 0..20 {
            let (dir, engine) = test_engine();
            let engine = Arc::new(engine);
            engine
                .put_machine("order", 1, &sample_definition())
                .unwrap();

            let writers: Vec<_> = (0..4)
                .map(|t| {
                    let engine = engine.clone();
                    std::thread::spawn(move || {
                        for n in 0..25 {
                            // Fails with MachineNotFound after the flush; fine.
                            let _ = engine.create_instance(
                                &format!("r{round}-t{t}-n{n}"),
                                "order",
                                1,
                                json!({}),
                                None,
                            );
                        }
                    })
                })
                .collect();
            engine.flush_all().unwrap();
            for w in writers {
                w.join().unwrap();
            }

            let mut live = engine.list_instance_ids();
            live.sort();
            drop(engine);
            let mut replayed = reopen_with(&dir, RecoveryState::default()).list_instance_ids();
            replayed.sort();
            assert_eq!(live, replayed, "round {round}");
        }
    }

    fn snapshot_of(engine: &StateMachineEngine, id: &str) -> InstanceSnapshot {
        let (instance, records) = engine
            .instances_for_snapshot()
            .into_iter()
            .find(|(i, _)| i.id == id)
            .unwrap();
        InstanceSnapshot::from_instance(&instance, format!("s-{id}")).with_idempotency(records)
    }

    /// Regression: a snapshot taken after FLUSH_ALL must survive recovery
    /// while the marker is still in the WAL.
    #[test]
    fn test_recovery_keeps_snapshot_taken_after_flush() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine.flush_all().unwrap();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("x", "order", 1, json!({"v": 1}), None)
            .unwrap();
        let recovery = RecoveryState {
            machines: engine.machine_records().unwrap(),
            snapshots: vec![snapshot_of(&engine, "x")],
            machines_wal_offset: engine.wal().latest_offset().unwrap().as_u64(),
        };
        drop(engine);

        let engine = reopen_with(&dir, recovery);
        assert_eq!(engine.get_instance("x").unwrap().ctx, json!({"v": 1}));
        assert!(engine.get_machine("order", 1).is_ok());
    }

    /// Crash between the marker and clearing the store: the stale snapshot
    /// and machines.json must be ignored.
    #[test]
    fn test_recovery_ignores_state_older_than_flush() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("old", "order", 1, json!({}), None)
            .unwrap();
        let stale = RecoveryState {
            machines: engine.machine_records().unwrap(),
            snapshots: vec![snapshot_of(&engine, "old")],
            machines_wal_offset: engine.wal().latest_offset().unwrap().as_u64(),
        };
        let flushed = engine.flush_all().unwrap();
        drop(engine);

        let engine = reopen_with(&dir, stale);
        assert!(engine.get_instance("old").is_err());
        assert!(engine.get_machine("order", 1).is_err());
        assert_eq!(engine.last_flush_offset(), Some(flushed.wal_offset));
    }

    #[test]
    fn test_recovery_after_two_flushes_keeps_only_latest_data() {
        let (dir, engine) = test_engine();
        for round in 0..2 {
            engine
                .put_machine("order", 1, &sample_definition())
                .unwrap();
            engine
                .create_instance(&format!("r{round}"), "order", 1, json!({}), None)
                .unwrap();
            engine.flush_all().unwrap();
        }
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("final", "order", 1, json!({}), None)
            .unwrap();
        drop(engine);

        let engine = reopen_with(&dir, RecoveryState::default());
        assert_eq!(engine.list_instance_ids(), vec!["final".to_string()]);
    }

    #[test]
    fn test_recreated_ids_after_flush_restore_new_state() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({"gen": 1}), Some("c"))
            .unwrap();
        engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("pay"))
            .unwrap();
        engine.flush_all().unwrap();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({"gen": 2}), None)
            .unwrap();
        drop(engine);

        let engine = reopen_with(&dir, RecoveryState::default());
        let i = engine.get_instance("i-1").unwrap();
        assert_eq!(i.state, "created");
        assert_eq!(i.ctx, json!({"gen": 2}));
        // The pre-flush idempotency key is forgotten.
        let r = engine
            .apply_event("i-1", "PAY", json!({}), None, None, None, Some("pay"))
            .unwrap();
        assert!(r.applied);
    }

    #[test]
    fn test_recovery_without_flush_uses_legacy_machines() {
        let (dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        let machines = engine.machine_records().unwrap();
        drop(engine);

        let engine = reopen_with(
            &dir,
            RecoveryState {
                machines,
                snapshots: vec![],
                machines_wal_offset: 0,
            },
        );
        assert!(engine.get_machine("order", 1).is_ok());
        assert_eq!(engine.last_flush_offset(), None);
    }

    #[test]
    fn test_replayed_flush_all_clears_state() {
        let (_dir, engine) = test_engine();
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("i-1", "order", 1, json!({}), None)
            .unwrap();

        engine
            .apply_replicated_entry(0, WalEntry::FlushAll { timestamp: 0 })
            .unwrap();

        assert_eq!(engine.instance_count(), 0);
        assert!(engine.get_machine("order", 1).is_err());
    }

    #[test]
    fn test_apply_replicated_put_machine() {
        let (_dir, engine) = test_engine();

        let entry = WalEntry::PutMachine {
            machine: "order".to_string(),
            version: 1,
            definition_hash: "abc123".to_string(),
            definition: sample_definition(),
        };

        let (seq, offset) = engine.apply_replicated_entry(0, entry).unwrap();
        assert_eq!(seq, 1);
        assert!(offset > 0);

        // Machine should now be available
        let def = engine.get_machine("order", 1).unwrap();
        assert_eq!(def.name, "order");
        assert_eq!(def.version, 1);
    }

    #[test]
    fn test_apply_replicated_create_instance() {
        let (_dir, engine) = test_engine();

        // First replicate the machine definition
        engine
            .apply_replicated_entry(
                0,
                WalEntry::PutMachine {
                    machine: "order".to_string(),
                    version: 1,
                    definition_hash: "abc".to_string(),
                    definition: sample_definition(),
                },
            )
            .unwrap();

        // Then replicate instance creation
        let (seq, offset) = engine
            .apply_replicated_entry(
                0,
                WalEntry::CreateInstance {
                    instance_id: "i-repl-1".to_string(),
                    machine: "order".to_string(),
                    version: 1,
                    initial_state: "created".to_string(),
                    initial_ctx: json!({"source": "primary"}),
                    idempotency_key: None,
                },
            )
            .unwrap();

        assert_eq!(seq, 2);
        assert!(offset > 0);

        let instance = engine.get_instance("i-repl-1").unwrap();
        assert_eq!(instance.state, "created");
        assert_eq!(instance.machine, "order");
        assert_eq!(instance.ctx, json!({"source": "primary"}));
    }

    #[test]
    fn test_apply_replicated_apply_event() {
        let (_dir, engine) = test_engine();

        // Set up machine + instance via replication
        engine
            .apply_replicated_entry(
                0,
                WalEntry::PutMachine {
                    machine: "order".to_string(),
                    version: 1,
                    definition_hash: "abc".to_string(),
                    definition: sample_definition(),
                },
            )
            .unwrap();

        engine
            .apply_replicated_entry(
                0,
                WalEntry::CreateInstance {
                    instance_id: "i-repl-2".to_string(),
                    machine: "order".to_string(),
                    version: 1,
                    initial_state: "created".to_string(),
                    initial_ctx: json!({}),
                    idempotency_key: None,
                },
            )
            .unwrap();

        // Replicate an event application
        let (seq, _) = engine
            .apply_replicated_entry(
                0,
                WalEntry::ApplyEvent {
                    instance_id: "i-repl-2".to_string(),
                    event: "PAY".to_string(),
                    from_state: "created".to_string(),
                    to_state: "paid".to_string(),
                    payload: json!({"amount": 42}),
                    ctx: json!({"amount": 42}),
                    event_id: Some("evt-1".to_string()),
                    idempotency_key: None,
                },
            )
            .unwrap();

        assert_eq!(seq, 3);

        let instance = engine.get_instance("i-repl-2").unwrap();
        assert_eq!(instance.state, "paid");
        assert_eq!(instance.ctx["amount"], 42);
        assert_eq!(instance.last_event_id, Some("evt-1".to_string()));
    }

    #[test]
    fn test_apply_replicated_delete_instance() {
        let (_dir, engine) = test_engine();

        engine
            .apply_replicated_entry(
                0,
                WalEntry::PutMachine {
                    machine: "order".to_string(),
                    version: 1,
                    definition_hash: "abc".to_string(),
                    definition: sample_definition(),
                },
            )
            .unwrap();

        engine
            .apply_replicated_entry(
                0,
                WalEntry::CreateInstance {
                    instance_id: "i-repl-del".to_string(),
                    machine: "order".to_string(),
                    version: 1,
                    initial_state: "created".to_string(),
                    initial_ctx: json!({}),
                    idempotency_key: None,
                },
            )
            .unwrap();

        // Instance exists
        assert!(engine.get_instance("i-repl-del").is_ok());

        // Replicate deletion
        let (seq, _) = engine
            .apply_replicated_entry(
                0,
                WalEntry::DeleteInstance {
                    instance_id: "i-repl-del".to_string(),
                    idempotency_key: None,
                },
            )
            .unwrap();

        assert_eq!(seq, 3);

        // Instance should be gone
        assert!(engine.get_instance("i-repl-del").is_err());
    }

    #[test]
    fn test_apply_replicated_entries_persist_to_wal() {
        let dir = TempDir::new().unwrap();

        // Apply entries on first engine
        {
            let config = WalConfig::new(dir.path())
                .with_segment_size(4096)
                .with_fsync_policy(FsyncPolicy::EveryWrite);
            let engine = StateMachineEngine::new(config).unwrap();

            engine
                .apply_replicated_entry(
                    0,
                    WalEntry::PutMachine {
                        machine: "order".to_string(),
                        version: 1,
                        definition_hash: "abc".to_string(),
                        definition: sample_definition(),
                    },
                )
                .unwrap();

            engine
                .apply_replicated_entry(
                    0,
                    WalEntry::CreateInstance {
                        instance_id: "i-persist".to_string(),
                        machine: "order".to_string(),
                        version: 1,
                        initial_state: "created".to_string(),
                        initial_ctx: json!({"replicated": true}),
                        idempotency_key: None,
                    },
                )
                .unwrap();

            engine
                .apply_replicated_entry(
                    0,
                    WalEntry::ApplyEvent {
                        instance_id: "i-persist".to_string(),
                        event: "PAY".to_string(),
                        from_state: "created".to_string(),
                        to_state: "paid".to_string(),
                        payload: json!({"amount": 100}),
                        ctx: json!({"replicated": true, "amount": 100}),
                        event_id: None,
                        idempotency_key: None,
                    },
                )
                .unwrap();
        }

        // Reopen engine — WAL replay should restore state
        {
            let config = WalConfig::new(dir.path())
                .with_segment_size(4096)
                .with_fsync_policy(FsyncPolicy::EveryWrite);
            let engine = StateMachineEngine::new(config).unwrap();

            let def = engine.get_machine("order", 1).unwrap();
            assert_eq!(def.name, "order");

            let instance = engine.get_instance("i-persist").unwrap();
            assert_eq!(instance.state, "paid");
            assert_eq!(instance.ctx["amount"], 100);
            assert_eq!(instance.ctx["replicated"], true);
        }
    }

    #[test]
    fn test_apply_replicated_full_sequence() {
        let (_dir, engine) = test_engine();

        // Replicate a full workflow: machine → instance → event → event → delete
        engine
            .apply_replicated_entry(
                0,
                WalEntry::PutMachine {
                    machine: "order".to_string(),
                    version: 1,
                    definition_hash: "abc".to_string(),
                    definition: sample_definition(),
                },
            )
            .unwrap();

        engine
            .apply_replicated_entry(
                0,
                WalEntry::CreateInstance {
                    instance_id: "i-full".to_string(),
                    machine: "order".to_string(),
                    version: 1,
                    initial_state: "created".to_string(),
                    initial_ctx: json!({"items_ready": true}),
                    idempotency_key: None,
                },
            )
            .unwrap();

        engine
            .apply_replicated_entry(
                0,
                WalEntry::ApplyEvent {
                    instance_id: "i-full".to_string(),
                    event: "PAY".to_string(),
                    from_state: "created".to_string(),
                    to_state: "paid".to_string(),
                    payload: json!({}),
                    ctx: json!({"items_ready": true}),
                    event_id: None,
                    idempotency_key: None,
                },
            )
            .unwrap();

        engine
            .apply_replicated_entry(
                0,
                WalEntry::ApplyEvent {
                    instance_id: "i-full".to_string(),
                    event: "SHIP".to_string(),
                    from_state: "paid".to_string(),
                    to_state: "shipped".to_string(),
                    payload: json!({"tracking": "XYZ"}),
                    ctx: json!({"items_ready": true, "tracking": "XYZ"}),
                    event_id: None,
                    idempotency_key: None,
                },
            )
            .unwrap();

        let instance = engine.get_instance("i-full").unwrap();
        assert_eq!(instance.state, "shipped");
        assert_eq!(instance.ctx["tracking"], "XYZ");

        engine
            .apply_replicated_entry(
                0,
                WalEntry::DeleteInstance {
                    instance_id: "i-full".to_string(),
                    idempotency_key: None,
                },
            )
            .unwrap();

        assert!(engine.get_instance("i-full").is_err());
    }

    #[test]
    fn test_apply_replicated_sequences_are_monotonic() {
        let (_dir, engine) = test_engine();

        let (seq1, off1) = engine
            .apply_replicated_entry(
                0,
                WalEntry::PutMachine {
                    machine: "m1".to_string(),
                    version: 1,
                    definition_hash: "a".to_string(),
                    definition: sample_definition(),
                },
            )
            .unwrap();

        let (seq2, off2) = engine
            .apply_replicated_entry(
                0,
                WalEntry::CreateInstance {
                    instance_id: "i1".to_string(),
                    machine: "m1".to_string(),
                    version: 1,
                    initial_state: "created".to_string(),
                    initial_ctx: json!({}),
                    idempotency_key: None,
                },
            )
            .unwrap();

        let (seq3, off3) = engine
            .apply_replicated_entry(
                0,
                WalEntry::ApplyEvent {
                    instance_id: "i1".to_string(),
                    event: "PAY".to_string(),
                    from_state: "created".to_string(),
                    to_state: "paid".to_string(),
                    payload: json!({}),
                    ctx: json!({}),
                    event_id: None,
                    idempotency_key: None,
                },
            )
            .unwrap();

        assert_eq!(seq1, 1);
        assert_eq!(seq2, 2);
        assert_eq!(seq3, 3);
        assert!(off1 < off2);
        assert!(off2 < off3);
    }

    #[test]
    fn test_apply_replicated_snapshot_and_checkpoint_are_noops() {
        let (_dir, engine) = test_engine();

        // Snapshot and Checkpoint entries should be accepted but not affect state
        let (seq1, _) = engine
            .apply_replicated_entry(
                0,
                WalEntry::Snapshot {
                    instance_id: "nonexistent".to_string(),
                    snapshot_id: "snap-1".to_string(),
                    state: "s".to_string(),
                    ctx: json!({}),
                },
            )
            .unwrap();

        let (seq2, _) = engine
            .apply_replicated_entry(0, WalEntry::Checkpoint { timestamp: 0 })
            .unwrap();

        assert_eq!(seq1, 1);
        assert_eq!(seq2, 2);
        assert_eq!(engine.instance_count(), 0);
    }

    // =========================================================================
    // Idempotency Key Limit Tests
    // =========================================================================

    fn loop_definition() -> Value {
        json!({
            "states": ["open"],
            "initial": "open",
            "transitions": [{"from": "open", "event": "TICK", "to": "open"}]
        })
    }

    fn loop_instance(engine: &StateMachineEngine, id: &str) {
        engine.put_machine("loop", 1, &loop_definition()).unwrap();
        engine
            .create_instance(id, "loop", 1, json!({}), None)
            .unwrap();
    }

    fn tick(engine: &StateMachineEngine, id: &str, key: &str) -> ApplyResult {
        engine
            .apply_event(id, "TICK", json!({}), None, None, None, Some(key))
            .unwrap()
    }

    fn cached_keys(engine: &StateMachineEngine, id: &str) -> Vec<String> {
        engine
            .idempotency_records(id)
            .into_iter()
            .map(|r| r.key)
            .collect()
    }

    #[test]
    fn test_key_limit_evicts_oldest_key() {
        let (_dir, engine) = test_engine();
        let engine = engine.with_max_idempotency_keys(3);
        loop_instance(&engine, "i-1");
        for i in 0..4 {
            assert!(tick(&engine, "i-1", &format!("k{i}")).applied);
        }

        assert_eq!(engine.idempotency_records("i-1").len(), 3);
        assert!(!tick(&engine, "i-1", "k3").applied);
        // k0 was evicted, so a retry with it is applied again.
        assert!(tick(&engine, "i-1", "k0").applied);
    }

    #[test]
    fn test_default_engine_has_no_key_limit() {
        let (_dir, engine) = test_engine();
        assert_eq!(engine.max_idempotency_keys(), 0);
        loop_instance(&engine, "i-1");
        for i in 0..50 {
            tick(&engine, "i-1", &format!("k{i}"));
        }
        assert_eq!(engine.idempotency_records("i-1").len(), 50);
    }

    #[test]
    fn test_key_limit_trims_after_recovery() {
        let (dir, engine) = test_engine();
        loop_instance(&engine, "i-1");
        for i in 0..5 {
            tick(&engine, "i-1", &format!("k{i}"));
        }
        drop(engine);

        let engine = reopen(&dir).with_max_idempotency_keys(2);
        assert_eq!(cached_keys(&engine, "i-1"), vec!["k3", "k4"]);
        assert!(!tick(&engine, "i-1", "k4").applied);
        assert!(tick(&engine, "i-1", "k2").applied);
    }

    #[test]
    fn test_key_limit_applies_during_recovery() {
        let (dir, engine) = test_engine();
        loop_instance(&engine, "i-1");
        for i in 0..5 {
            tick(&engine, "i-1", &format!("k{i}"));
        }
        let snapshot = snapshot_of(&engine, "i-1");
        let machines = engine.machine_records().unwrap();
        let head = engine.wal().latest_offset().unwrap().as_u64();
        for i in 5..8 {
            tick(&engine, "i-1", &format!("k{i}"));
        }
        drop(engine);

        let config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        let recovery = RecoveryState {
            machines,
            snapshots: vec![snapshot],
            machines_wal_offset: head,
        };
        let engine = StateMachineEngine::new_with_options(
            config,
            recovery,
            EngineOptions {
                max_idempotency_keys: 2,
            },
        )
        .unwrap();
        assert_eq!(engine.max_idempotency_keys(), 2);
        assert_eq!(cached_keys(&engine, "i-1"), vec!["k6", "k7"]);
        assert!(tick(&engine, "i-1", "k5").applied);
    }

    #[test]
    fn test_key_limit_trims_oversized_snapshot() {
        let (dir, engine) = test_engine();
        loop_instance(&engine, "i-1");
        for i in 0..5 {
            tick(&engine, "i-1", &format!("k{i}"));
        }
        let snapshot = snapshot_of(&engine, "i-1");
        let machines = engine.machine_records().unwrap();
        let head = engine.wal().latest_offset().unwrap().as_u64();
        drop(engine);

        let recovery = RecoveryState {
            machines,
            snapshots: vec![snapshot],
            machines_wal_offset: head,
        };
        let engine = reopen_with(&dir, recovery).with_max_idempotency_keys(2);
        assert_eq!(cached_keys(&engine, "i-1"), vec!["k3", "k4"]);
    }

    #[test]
    fn test_reapplied_evicted_key_is_cached_once() {
        let (dir, engine) = test_engine();
        let engine = engine.with_max_idempotency_keys(2);
        loop_instance(&engine, "i-1");
        for key in ["k0", "k1", "k2"] {
            tick(&engine, "i-1", key);
        }
        assert!(tick(&engine, "i-1", "k0").applied);
        assert_eq!(cached_keys(&engine, "i-1"), vec!["k2", "k0"]);
        drop(engine);

        // Without a limit, replay sees k0 twice in the WAL; it must still be
        // cached once.
        let unlimited = reopen(&dir);
        assert_eq!(cached_keys(&unlimited, "i-1"), vec!["k1", "k2", "k0"]);
        drop(unlimited);

        let engine = reopen(&dir).with_max_idempotency_keys(2);
        assert_eq!(cached_keys(&engine, "i-1"), vec!["k2", "k0"]);
        assert!(!tick(&engine, "i-1", "k0").applied);
        assert!(tick(&engine, "i-1", "k1").applied);
    }

    #[test]
    fn test_replicated_idempotent_retry_reports_local_sequence() {
        let (_dir, engine) = test_engine();
        engine
            .apply_replicated_entry(
                1,
                WalEntry::PutMachine {
                    machine: "loop".to_string(),
                    version: 1,
                    definition_hash: "abc".to_string(),
                    definition: loop_definition(),
                },
            )
            .unwrap();
        engine
            .apply_replicated_entry(
                2,
                WalEntry::CreateInstance {
                    instance_id: "i-1".to_string(),
                    machine: "loop".to_string(),
                    version: 1,
                    initial_state: "open".to_string(),
                    initial_ctx: json!({}),
                    idempotency_key: None,
                },
            )
            .unwrap();
        let (seq, _) = engine
            .apply_replicated_entry(
                3,
                WalEntry::ApplyEvent {
                    instance_id: "i-1".to_string(),
                    event: "TICK".to_string(),
                    from_state: "open".to_string(),
                    to_state: "open".to_string(),
                    payload: json!({}),
                    ctx: json!({}),
                    event_id: None,
                    idempotency_key: Some("k".to_string()),
                },
            )
            .unwrap();

        // After promotion, a retry must report the event's real sequence.
        let retry = tick(&engine, "i-1", "k");
        assert!(!retry.applied);
        assert_eq!(retry.sequence, seq);
        assert_eq!(engine.idempotency_records("i-1")[0].sequence, seq);
    }

    #[test]
    fn test_replicated_entries_respect_key_limit() {
        let (_dir, engine) = test_engine();
        let engine = engine.with_max_idempotency_keys(2);
        engine
            .apply_replicated_entry(
                1,
                WalEntry::PutMachine {
                    machine: "loop".to_string(),
                    version: 1,
                    definition_hash: "abc".to_string(),
                    definition: loop_definition(),
                },
            )
            .unwrap();
        engine
            .apply_replicated_entry(
                2,
                WalEntry::CreateInstance {
                    instance_id: "i-1".to_string(),
                    machine: "loop".to_string(),
                    version: 1,
                    initial_state: "open".to_string(),
                    initial_ctx: json!({}),
                    idempotency_key: None,
                },
            )
            .unwrap();
        for i in 0..4u64 {
            engine
                .apply_replicated_entry(
                    10 + i,
                    WalEntry::ApplyEvent {
                        instance_id: "i-1".to_string(),
                        event: "TICK".to_string(),
                        from_state: "open".to_string(),
                        to_state: "open".to_string(),
                        payload: json!({}),
                        ctx: json!({}),
                        event_id: None,
                        idempotency_key: Some(format!("k{i}")),
                    },
                )
                .unwrap();
        }
        assert_eq!(cached_keys(&engine, "i-1"), vec!["k2", "k3"]);
    }

    #[test]
    fn test_snapshot_records_respect_key_limit() {
        let (_dir, engine) = test_engine();
        let engine = engine.with_max_idempotency_keys(3);
        loop_instance(&engine, "i-1");
        for i in 0..5 {
            tick(&engine, "i-1", &format!("k{i}"));
        }
        let snapshot = snapshot_of(&engine, "i-1");
        let keys: Vec<&str> = snapshot
            .idempotency
            .iter()
            .map(|r| r.key.as_str())
            .collect();
        assert_eq!(keys, vec!["k2", "k3", "k4"]);
    }

    #[test]
    fn test_key_limit_zero_means_unlimited() {
        let (_dir, engine) = test_engine();
        let engine = engine.with_max_idempotency_keys(0);
        loop_instance(&engine, "i-1");
        for i in 0..20 {
            tick(&engine, "i-1", &format!("k{i}"));
        }
        assert_eq!(engine.idempotency_records("i-1").len(), 20);
    }
}
