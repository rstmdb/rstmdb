//! Command handlers.

use crate::auth::TokenValidator;
use crate::broadcast::{EventBroadcaster, EventFilter, InstanceEvent};
use crate::config::AuthConfig;
use crate::error::ServerError;
use crate::metrics::Metrics;
use crate::session::{Session, SessionState, WireMode};
use rstmdb_core::instance::InstanceSnapshot;
use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::message::*;
use rstmdb_protocol::ErrorCode;
use rstmdb_protocol::PROTOCOL_VERSION;
use rstmdb_storage::SnapshotStore;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Server capabilities and limits.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    pub name: String,
    pub version: String,
    pub features: Vec<String>,
    pub max_frame_bytes: u32,
    pub max_batch_ops: u32,
}

impl Default for ServerInfo {
    fn default() -> Self {
        Self {
            name: "rstmdb".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            features: vec![
                "idempotency".to_string(),
                "batch".to_string(),
                "wal_read".to_string(),
            ],
            max_frame_bytes: 16 * 1024 * 1024,
            max_batch_ops: 100,
        }
    }
}

/// Command handler.
pub struct CommandHandler {
    engine: Arc<StateMachineEngine>,
    snapshot_store: Option<Arc<SnapshotStore>>,
    info: ServerInfo,
    /// Token validator for authentication.
    token_validator: Option<TokenValidator>,
    /// Whether authentication is required.
    auth_required: bool,
    /// Maximum number of versions per machine (0 = unlimited).
    max_machine_versions: u32,
    /// Event broadcaster for watch subscriptions.
    broadcaster: Option<Arc<EventBroadcaster>>,
    /// Metrics for request tracking.
    metrics: Option<Arc<Metrics>>,
}

impl CommandHandler {
    /// Creates a new command handler.
    pub fn new(engine: Arc<StateMachineEngine>) -> Self {
        Self {
            engine,
            snapshot_store: None,
            info: ServerInfo::default(),
            token_validator: None,
            auth_required: false,
            max_machine_versions: 0,
            broadcaster: None,
            metrics: None,
        }
    }

    /// Creates a new command handler with authentication.
    pub fn with_auth(engine: Arc<StateMachineEngine>, auth_config: &AuthConfig) -> Self {
        let token_validator = if auth_config.required && !auth_config.token_hashes.is_empty() {
            Some(TokenValidator::new(auth_config.token_hashes.clone()))
        } else {
            None
        };

        Self {
            engine,
            snapshot_store: None,
            info: ServerInfo::default(),
            token_validator,
            auth_required: auth_config.required,
            max_machine_versions: 0,
            broadcaster: None,
            metrics: None,
        }
    }

    /// Creates a new command handler with snapshot support.
    pub fn with_snapshots(
        engine: Arc<StateMachineEngine>,
        snapshot_dir: impl AsRef<Path>,
    ) -> Result<Self, ServerError> {
        let snapshot_store = SnapshotStore::open(snapshot_dir)?;
        Ok(Self {
            engine,
            snapshot_store: Some(Arc::new(snapshot_store)),
            info: ServerInfo::default(),
            token_validator: None,
            auth_required: false,
            max_machine_versions: 0,
            broadcaster: None,
            metrics: None,
        })
    }

    /// Creates a new command handler with snapshots and authentication.
    pub fn with_snapshots_and_auth(
        engine: Arc<StateMachineEngine>,
        snapshot_dir: impl AsRef<Path>,
        auth_config: &AuthConfig,
    ) -> Result<Self, ServerError> {
        let snapshot_store = SnapshotStore::open(snapshot_dir)?;
        let token_validator = if auth_config.required && !auth_config.token_hashes.is_empty() {
            Some(TokenValidator::new(auth_config.token_hashes.clone()))
        } else {
            None
        };

        Ok(Self {
            engine,
            snapshot_store: Some(Arc::new(snapshot_store)),
            info: ServerInfo::default(),
            token_validator,
            auth_required: auth_config.required,
            max_machine_versions: 0,
            broadcaster: None,
            metrics: None,
        })
    }

    /// Creates a new command handler with custom server info.
    pub fn with_info(engine: Arc<StateMachineEngine>, info: ServerInfo) -> Self {
        Self {
            engine,
            snapshot_store: None,
            info,
            token_validator: None,
            auth_required: false,
            max_machine_versions: 0,
            broadcaster: None,
            metrics: None,
        }
    }

    /// Sets the event broadcaster for watch subscriptions.
    pub fn with_broadcaster(mut self, broadcaster: Arc<EventBroadcaster>) -> Self {
        self.broadcaster = Some(broadcaster);
        self
    }

    /// Sets the metrics instance.
    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Sets the maximum number of versions per machine (0 = unlimited).
    pub fn with_max_machine_versions(mut self, max: u32) -> Self {
        self.max_machine_versions = max;
        self
    }

    /// Returns a reference to the broadcaster, if set.
    pub fn broadcaster(&self) -> Option<&Arc<EventBroadcaster>> {
        self.broadcaster.as_ref()
    }

    /// Returns a reference to the metrics, if set.
    pub fn metrics(&self) -> Option<&Arc<Metrics>> {
        self.metrics.as_ref()
    }

    /// Updates gauge metrics from current engine state.
    pub fn update_gauge_metrics(&self) {
        if let Some(ref metrics) = self.metrics {
            // Update instances count
            let instances = self.engine.get_all_instances();
            metrics.instances_total.set(instances.len() as f64);

            // Update machines count
            let machines = self.engine.list_machines();
            let machine_count: usize = machines.values().map(|versions| versions.len()).sum();
            metrics.machines_total.set(machine_count as f64);

            // Update WAL metrics
            let wal = self.engine.wal();
            // next_sequence is 1-based, so subtract 1 to get actual entry count
            let entry_count = wal.next_sequence().saturating_sub(1);
            metrics.wal_entries.set(entry_count as f64);
            metrics.wal_segments.set(wal.segment_ids().len() as f64);
            metrics.wal_size_bytes.set(wal.total_size() as f64);

            // Update WAL I/O counters
            let wal_stats = wal.stats();
            metrics.update_wal_stats(wal_stats);
        }
    }

    /// Returns whether authentication is required for an operation.
    fn requires_auth(&self, op: &Operation) -> bool {
        if !self.auth_required {
            return false;
        }
        // Commands that do NOT require auth (even when auth_required=true)
        !matches!(
            op,
            Operation::Hello |   // Must complete handshake
            Operation::Auth |    // Must be able to authenticate
            Operation::Ping |    // Health checks should work
            Operation::Bye // Graceful disconnect
        )
    }

    /// Handles a request and returns a response.
    pub fn handle(&self, session: &mut Session, request: &Request) -> Response {
        session.record_request();

        let op_name = Self::operation_name(&request.op);

        // Start timing for metrics
        let timer = self.metrics.as_ref().map(|m| {
            m.request_duration
                .with_label_values(&[op_name])
                .start_timer()
        });

        // AUTH ENFORCEMENT: Check if command requires authentication
        if self.requires_auth(&request.op) && !session.is_authenticated() {
            // Record metrics
            if let Some(ref metrics) = self.metrics {
                metrics.requests_total.with_label_values(&[op_name]).inc();
                metrics
                    .errors_total
                    .with_label_values(&["UNAUTHORIZED"])
                    .inc();
            }
            drop(timer); // Observation happens on drop
            return Response::error(
                &request.id,
                ResponseError::new(
                    ErrorCode::Unauthorized,
                    "authentication required".to_string(),
                ),
            );
        }

        let result = match request.op {
            Operation::Hello => self.handle_hello(session, &request.params),
            Operation::Auth => self.handle_auth(session, &request.params),
            Operation::Ping => self.handle_ping(),
            Operation::Bye => self.handle_bye(session),
            Operation::Info => self.handle_info(),
            Operation::PutMachine => self.handle_put_machine(&request.params),
            Operation::GetMachine => self.handle_get_machine(&request.params),
            Operation::ListMachines => self.handle_list_machines(&request.params),
            Operation::CreateInstance => self.handle_create_instance(&request.params),
            Operation::GetInstance => self.handle_get_instance(&request.params),
            Operation::ListInstances => self.handle_list_instances(&request.params),
            Operation::DeleteInstance => self.handle_delete_instance(&request.params),
            Operation::ApplyEvent => self.handle_apply_event(&request.params),
            Operation::Batch => self.handle_batch(session, &request.params),
            Operation::SnapshotInstance => self.handle_snapshot_instance(&request.params),
            Operation::WalRead => self.handle_wal_read(&request.params),
            Operation::WalStats => self.handle_wal_stats(),
            Operation::Compact => self.handle_compact(&request.params),
            Operation::WatchInstance => self.handle_watch_instance_cmd(session, &request.params),
            Operation::WatchAll => self.handle_watch_all_cmd(session, &request.params),
            Operation::Unwatch => self.handle_unwatch(session, &request.params),
        };

        // Record metrics
        if let Some(ref metrics) = self.metrics {
            metrics.requests_total.with_label_values(&[op_name]).inc();
            if let Err(ref e) = result {
                let error_code = Self::error_code_name(e.error_code());
                metrics.errors_total.with_label_values(&[error_code]).inc();
            }
        }
        drop(timer); // Observation happens on drop

        match result {
            Ok(value) => Response::ok(&request.id, value),
            Err(e) => Response::error(
                &request.id,
                ResponseError::new(e.error_code(), e.to_string()),
            ),
        }
    }

    /// Returns the string name for an operation.
    fn operation_name(op: &Operation) -> &'static str {
        match op {
            Operation::Hello => "HELLO",
            Operation::Auth => "AUTH",
            Operation::Ping => "PING",
            Operation::Bye => "BYE",
            Operation::Info => "INFO",
            Operation::PutMachine => "PUT_MACHINE",
            Operation::GetMachine => "GET_MACHINE",
            Operation::ListMachines => "LIST_MACHINES",
            Operation::CreateInstance => "CREATE_INSTANCE",
            Operation::GetInstance => "GET_INSTANCE",
            Operation::ListInstances => "LIST_INSTANCES",
            Operation::DeleteInstance => "DELETE_INSTANCE",
            Operation::ApplyEvent => "APPLY_EVENT",
            Operation::Batch => "BATCH",
            Operation::SnapshotInstance => "SNAPSHOT_INSTANCE",
            Operation::WalRead => "WAL_READ",
            Operation::WalStats => "WAL_STATS",
            Operation::Compact => "COMPACT",
            Operation::WatchInstance => "WATCH_INSTANCE",
            Operation::WatchAll => "WATCH_ALL",
            Operation::Unwatch => "UNWATCH",
        }
    }

    /// Returns the string name for an error code.
    fn error_code_name(code: ErrorCode) -> &'static str {
        match code {
            ErrorCode::UnsupportedProtocol => "UNSUPPORTED_PROTOCOL",
            ErrorCode::BadRequest => "BAD_REQUEST",
            ErrorCode::Unauthorized => "UNAUTHORIZED",
            ErrorCode::AuthFailed => "AUTH_FAILED",
            ErrorCode::NotFound => "NOT_FOUND",
            ErrorCode::MachineNotFound => "MACHINE_NOT_FOUND",
            ErrorCode::MachineVersionExists => "MACHINE_VERSION_EXISTS",
            ErrorCode::MachineVersionLimitExceeded => "MACHINE_VERSION_LIMIT_EXCEEDED",
            ErrorCode::InstanceNotFound => "INSTANCE_NOT_FOUND",
            ErrorCode::InstanceExists => "INSTANCE_EXISTS",
            ErrorCode::InvalidTransition => "INVALID_TRANSITION",
            ErrorCode::GuardFailed => "GUARD_FAILED",
            ErrorCode::Conflict => "CONFLICT",
            ErrorCode::WalIoError => "WAL_IO_ERROR",
            ErrorCode::InternalError => "INTERNAL_ERROR",
            ErrorCode::RateLimited => "RATE_LIMITED",
        }
    }

    fn handle_hello(&self, session: &mut Session, params: &Value) -> Result<Value, ServerError> {
        let hello: HelloParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        // Check protocol version
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(ServerError::InvalidRequest(format!(
                "unsupported protocol version: {}",
                hello.protocol_version
            )));
        }

        // Negotiate wire mode
        let wire_mode = if hello.wire_modes.contains(&"binary_json".to_string()) {
            WireMode::BinaryJson
        } else if hello.wire_modes.contains(&"jsonl".to_string()) {
            WireMode::Jsonl
        } else {
            WireMode::BinaryJson
        };

        // Negotiate features
        let supported_features: HashSet<_> = self.info.features.iter().cloned().collect();
        let client_features: HashSet<_> = hello.features.into_iter().collect();
        let negotiated: HashSet<_> = supported_features
            .intersection(&client_features)
            .cloned()
            .collect();

        session.complete_handshake(
            hello.protocol_version,
            wire_mode,
            hello.client_name,
            negotiated.clone(),
        );

        let result = HelloResult {
            protocol_version: PROTOCOL_VERSION,
            wire_mode: match wire_mode {
                WireMode::BinaryJson => "binary_json".to_string(),
                WireMode::Jsonl => "jsonl".to_string(),
            },
            server_name: self.info.name.clone(),
            server_version: self.info.version.clone(),
            features: negotiated.into_iter().collect(),
        };

        Ok(serde_json::to_value(result)?)
    }

    fn handle_auth(&self, session: &mut Session, params: &Value) -> Result<Value, ServerError> {
        let auth: AuthParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        // Validate method
        if auth.method != "bearer" {
            return Err(ServerError::AuthFailed(format!(
                "unsupported auth method: {}",
                auth.method
            )));
        }

        // If no validator configured, auth is disabled - accept any non-empty token
        // (This provides backward compatibility when auth_required=false)
        if self.token_validator.is_none() {
            if !auth.token.is_empty() {
                session.set_authenticated(true);
                session.set_state(SessionState::Authenticated);
                return Ok(json!({"authenticated": true}));
            } else {
                return Err(ServerError::AuthFailed("empty token".to_string()));
            }
        }

        // Validate token against configured hashes
        let validator = self.token_validator.as_ref().unwrap();
        if validator.validate(&auth.token) {
            session.set_authenticated(true);
            session.set_state(SessionState::Authenticated);
            Ok(json!({"authenticated": true}))
        } else {
            Err(ServerError::AuthFailed("invalid token".to_string()))
        }
    }

    fn handle_ping(&self) -> Result<Value, ServerError> {
        Ok(json!({"pong": true}))
    }

    fn handle_bye(&self, session: &mut Session) -> Result<Value, ServerError> {
        session.set_state(SessionState::Closing);
        Ok(json!({"goodbye": true}))
    }

    fn handle_info(&self) -> Result<Value, ServerError> {
        Ok(json!({
            "server_name": self.info.name,
            "server_version": self.info.version,
            "protocol_version": PROTOCOL_VERSION,
            "features": self.info.features,
            "max_frame_bytes": self.info.max_frame_bytes,
            "max_batch_ops": self.info.max_batch_ops,
        }))
    }

    fn handle_put_machine(&self, params: &Value) -> Result<Value, ServerError> {
        let p: PutMachineParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        // Check if version limit would be exceeded
        if self.max_machine_versions > 0 {
            let versions = self.engine.get_machine_versions(&p.machine);
            // Only check if this is a new version (not an update to existing)
            if !versions.contains(&p.version)
                && versions.len() >= self.max_machine_versions as usize
            {
                return Err(ServerError::MachineVersionLimitExceeded(format!(
                    "machine '{}' already has {} versions (limit: {})",
                    p.machine,
                    versions.len(),
                    self.max_machine_versions
                )));
            }
        }

        let (checksum, created) = self
            .engine
            .put_machine(&p.machine, p.version, &p.definition)?;

        // Update gauge metrics
        self.update_gauge_metrics();

        let result = PutMachineResult {
            machine: p.machine,
            version: p.version,
            stored_checksum: checksum,
            created,
        };

        Ok(serde_json::to_value(result)?)
    }

    fn handle_get_machine(&self, params: &Value) -> Result<Value, ServerError> {
        let p: GetMachineParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        let definition = self.engine.get_machine(&p.machine, p.version)?;

        let result = GetMachineResult {
            definition: definition.to_json(),
            checksum: definition.checksum.clone(),
        };

        Ok(serde_json::to_value(result)?)
    }

    fn handle_list_machines(&self, _params: &Value) -> Result<Value, ServerError> {
        let machines = self.engine.list_machines();
        let mut items: Vec<_> = machines
            .into_iter()
            .map(|(name, versions)| {
                json!({
                    "machine": name,
                    "versions": versions,
                })
            })
            .collect();

        // Sort machines alphabetically by name
        items.sort_by(|a, b| {
            let name_a = a["machine"].as_str().unwrap_or("");
            let name_b = b["machine"].as_str().unwrap_or("");
            name_a.cmp(name_b)
        });

        Ok(json!({
            "items": items,
        }))
    }

    fn handle_create_instance(&self, params: &Value) -> Result<Value, ServerError> {
        let p: CreateInstanceParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        let instance_id = p
            .instance_id
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let (instance, _seq) = self.engine.create_instance(
            &instance_id,
            &p.machine,
            p.version,
            p.initial_ctx,
            p.idempotency_key.as_deref(),
        )?;

        // Update gauge metrics
        self.update_gauge_metrics();

        let result = CreateInstanceResult {
            instance_id: instance.id,
            state: instance.state,
            wal_offset: instance.last_wal_offset,
        };

        Ok(serde_json::to_value(result)?)
    }

    fn handle_get_instance(&self, params: &Value) -> Result<Value, ServerError> {
        let p: GetInstanceParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        let instance = self.engine.get_instance(&p.instance_id)?;

        let result = GetInstanceResult {
            machine: instance.machine,
            version: instance.version,
            state: instance.state,
            ctx: instance.ctx,
            last_event_id: instance.last_event_id,
            last_wal_offset: instance.last_wal_offset,
        };

        Ok(serde_json::to_value(result)?)
    }

    fn handle_list_instances(&self, params: &Value) -> Result<Value, ServerError> {
        let p: ListInstancesParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        let all_instances = self.engine.get_all_instances();

        // Apply filters
        let filtered: Vec<_> = all_instances
            .into_iter()
            .filter(|i| {
                // Filter by machine if specified
                if let Some(ref machine) = p.machine {
                    if &i.machine != machine {
                        return false;
                    }
                }
                // Filter by state if specified
                if let Some(ref state) = p.state {
                    if &i.state != state {
                        return false;
                    }
                }
                true
            })
            .collect();

        let total = filtered.len() as u64;

        // Apply pagination
        let offset = p.offset.unwrap_or(0) as usize;
        let limit = p.limit.unwrap_or(100) as usize;

        let paginated: Vec<_> = filtered.into_iter().skip(offset).take(limit).collect();

        let has_more = (offset + paginated.len()) < total as usize;

        // Convert to summaries (without ctx for efficiency)
        let instances: Vec<InstanceSummary> = paginated
            .into_iter()
            .map(|i| InstanceSummary {
                id: i.id,
                machine: i.machine,
                version: i.version,
                state: i.state,
                created_at: i.created_at,
                updated_at: i.updated_at,
                last_wal_offset: i.last_wal_offset,
            })
            .collect();

        let result = ListInstancesResult {
            instances,
            total,
            has_more,
        };

        Ok(serde_json::to_value(result)?)
    }

    fn handle_delete_instance(&self, params: &Value) -> Result<Value, ServerError> {
        let instance_id = params["instance_id"]
            .as_str()
            .ok_or_else(|| ServerError::InvalidRequest("missing instance_id".to_string()))?;

        let idempotency_key = params["idempotency_key"].as_str();

        let wal_offset = self.engine.delete_instance(instance_id, idempotency_key)?;

        // Update gauge metrics
        self.update_gauge_metrics();

        Ok(json!({
            "instance_id": instance_id,
            "deleted": true,
            "wal_offset": wal_offset,
        }))
    }

    fn handle_apply_event(&self, params: &Value) -> Result<Value, ServerError> {
        let p: ApplyEventParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        // Get instance info before applying (for notification)
        let instance = self.engine.get_instance(&p.instance_id)?;

        let result = self.engine.apply_event(
            &p.instance_id,
            &p.event,
            p.payload.clone(),
            p.expected_state.as_deref(),
            p.expected_wal_offset,
            p.event_id.as_deref(),
            p.idempotency_key.as_deref(),
        )?;

        // Notify watchers if event was actually applied
        if result.applied {
            if let Some(broadcaster) = &self.broadcaster {
                broadcaster.notify(InstanceEvent {
                    instance_id: p.instance_id.clone(),
                    machine: instance.machine.clone(),
                    version: instance.version,
                    wal_offset: result.wal_offset,
                    from_state: result.from_state.clone(),
                    to_state: result.to_state.clone(),
                    event: p.event.clone(),
                    payload: p.payload,
                    ctx: result.ctx.clone(),
                });
            }

            // Update gauge metrics after successful apply
            self.update_gauge_metrics();
        }

        let apply_result = ApplyEventResult {
            from_state: result.from_state,
            to_state: result.to_state,
            ctx: Some(result.ctx),
            wal_offset: result.wal_offset,
            applied: result.applied,
            event_id: p.event_id,
        };

        Ok(serde_json::to_value(apply_result)?)
    }

    fn handle_batch(&self, session: &mut Session, params: &Value) -> Result<Value, ServerError> {
        let mode = params["mode"].as_str().unwrap_or("best_effort");
        let ops = params["ops"]
            .as_array()
            .ok_or_else(|| ServerError::InvalidRequest("missing ops array".to_string()))?;

        if ops.len() > self.info.max_batch_ops as usize {
            return Err(ServerError::InvalidRequest(format!(
                "batch size {} exceeds limit {}",
                ops.len(),
                self.info.max_batch_ops
            )));
        }

        let mut results = Vec::new();

        for op in ops {
            let op_type = op["op"]
                .as_str()
                .ok_or_else(|| ServerError::InvalidRequest("missing op type".to_string()))?;

            let request = Request {
                msg_type: "request".to_string(),
                id: "batch".to_string(),
                op: serde_json::from_value(json!(op_type))
                    .map_err(|e| ServerError::InvalidRequest(e.to_string()))?,
                params: op["params"].clone(),
            };

            let response = self.handle(session, &request);

            if mode == "atomic" && response.is_error() {
                // Atomic mode: fail entire batch on first error
                return Err(ServerError::InvalidRequest(format!(
                    "atomic batch failed: {:?}",
                    response.error
                )));
            }

            results.push(json!({
                "status": if response.is_ok() { "ok" } else { "error" },
                "result": response.result,
                "error": response.error,
            }));
        }

        Ok(json!({
            "results": results,
        }))
    }

    fn handle_snapshot_instance(&self, params: &Value) -> Result<Value, ServerError> {
        let instance_id = params["instance_id"]
            .as_str()
            .ok_or_else(|| ServerError::InvalidRequest("missing instance_id".to_string()))?;

        let instance = self.engine.get_instance(instance_id)?;
        let snapshot_id = format!("snap-{}", uuid::Uuid::new_v4());

        // If we have a snapshot store, persist the snapshot
        if let Some(store) = &self.snapshot_store {
            let snapshot = InstanceSnapshot::from_instance(&instance, &snapshot_id);
            let meta = store.create_snapshot(&snapshot)?;

            Ok(json!({
                "instance_id": instance_id,
                "snapshot_id": meta.snapshot_id,
                "wal_offset": meta.wal_offset,
                "size_bytes": meta.size_bytes,
                "checksum": meta.checksum,
            }))
        } else {
            // No snapshot store configured, return basic info
            Ok(json!({
                "instance_id": instance_id,
                "snapshot_id": snapshot_id,
                "wal_offset": instance.last_wal_offset,
            }))
        }
    }

    fn handle_compact(&self, params: &Value) -> Result<Value, ServerError> {
        let force_snapshot = params["force_snapshot"].as_bool().unwrap_or(false);

        let snapshot_store = self.snapshot_store.as_ref().ok_or_else(|| {
            ServerError::InvalidRequest("snapshot store not configured".to_string())
        })?;

        let mut snapshots_created = 0;

        // Snapshot instances that have changed since last snapshot
        for instance in self.engine.get_all_instances() {
            let needs_snapshot = if force_snapshot {
                // Check if instance changed since last snapshot
                match snapshot_store.get_snapshot_meta(&instance.id) {
                    Some(meta) => instance.last_wal_offset > meta.wal_offset,
                    None => true, // No snapshot exists
                }
            } else {
                // Only snapshot if no snapshot exists
                snapshot_store.get_snapshot_meta(&instance.id).is_none()
            };

            if needs_snapshot {
                let snapshot_id = format!("snap-{}", uuid::Uuid::new_v4());
                let snapshot = InstanceSnapshot::from_instance(&instance, snapshot_id);
                snapshot_store.create_snapshot(&snapshot)?;
                snapshots_created += 1;
            }
        }

        // Compact WAL segments before the minimum snapshot offset
        // Only segments that are ENTIRELY before min_offset can be deleted
        let (segments_deleted, bytes_reclaimed) =
            if let Some(min_offset) = snapshot_store.min_wal_offset() {
                let wal = self.engine.wal();
                let deleted = wal.compact_before(rstmdb_wal::WalOffset::from_u64(min_offset))?;
                // Estimate bytes reclaimed (segment_size * deleted_count)
                let bytes = (deleted as u64) * 64 * 1024 * 1024; // Approximate
                (deleted, bytes)
            } else {
                (0, 0)
            };

        // Report current state
        let total_snapshots = snapshot_store.snapshot_count();
        let wal_segments = self.engine.wal().segment_ids().len();

        Ok(json!({
            "snapshots_created": snapshots_created,
            "segments_deleted": segments_deleted,
            "bytes_reclaimed": bytes_reclaimed,
            "total_snapshots": total_snapshots,
            "wal_segments": wal_segments,
        }))
    }

    fn handle_wal_read(&self, params: &Value) -> Result<Value, ServerError> {
        let from_offset = params["from_offset"]
            .as_u64()
            .ok_or_else(|| ServerError::InvalidRequest("missing from_offset".to_string()))?;

        let limit = params["limit"].as_u64().map(|l| l as usize);

        let wal = self.engine.wal();
        let entries = wal.read_from(rstmdb_wal::WalOffset::from_u64(from_offset), limit)?;

        let records: Vec<_> = entries
            .into_iter()
            .map(|(seq, offset, entry)| {
                json!({
                    "sequence": seq,
                    "offset": offset.as_u64(),
                    "entry": entry,
                })
            })
            .collect();

        let next_offset = records.last().map(|r| r["offset"].as_u64().unwrap() + 1);

        Ok(json!({
            "records": records,
            "next_offset": next_offset,
        }))
    }

    fn handle_wal_stats(&self) -> Result<Value, ServerError> {
        let wal = self.engine.wal();
        let stats = wal.stats();
        let segment_ids = wal.segment_ids();
        let total_size = wal.total_size();
        let next_sequence = wal.next_sequence();
        // Entry count is next_sequence - 1 (sequence starts at 1)
        let entry_count = next_sequence.saturating_sub(1);
        let latest_offset = wal.latest_offset().map(|o| o.as_u64());

        Ok(json!({
            "entry_count": entry_count,
            "segment_count": segment_ids.len(),
            "total_size_bytes": total_size,
            "latest_offset": latest_offset,
            "io_stats": {
                "bytes_written": stats.bytes_written,
                "bytes_read": stats.bytes_read,
                "writes": stats.writes,
                "reads": stats.reads,
                "fsyncs": stats.fsyncs,
            }
        }))
    }

    /// Internal handler for WATCH_INSTANCE that returns the receiver.
    fn handle_watch_instance_cmd(
        &self,
        session: &mut Session,
        params: &Value,
    ) -> Result<Value, ServerError> {
        let p: WatchInstanceParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        // Verify instance exists and get current state
        let instance = self.engine.get_instance(&p.instance_id)?;

        let broadcaster = self.broadcaster.as_ref().ok_or_else(|| {
            ServerError::InvalidRequest("streaming not enabled on this server".to_string())
        })?;

        let (subscription_id, _receiver) =
            broadcaster.subscribe_instance(&p.instance_id, p.include_ctx);

        session.add_instance_subscription(subscription_id.clone(), p.instance_id.clone());

        let result = WatchInstanceResult {
            subscription_id,
            instance_id: p.instance_id,
            current_state: instance.state,
            current_wal_offset: instance.last_wal_offset,
        };

        Ok(serde_json::to_value(result)?)
    }

    /// Handles WATCH_INSTANCE and returns the receiver for streaming.
    /// This is called by the server to get both the response and the receiver.
    pub fn handle_watch_instance(
        &self,
        session: &mut Session,
        params: &Value,
    ) -> Result<(Value, broadcast::Receiver<InstanceEvent>), ServerError> {
        let p: WatchInstanceParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        // Verify instance exists and get current state
        let instance = self.engine.get_instance(&p.instance_id)?;

        let broadcaster = self.broadcaster.as_ref().ok_or_else(|| {
            ServerError::InvalidRequest("streaming not enabled on this server".to_string())
        })?;

        let (subscription_id, receiver) =
            broadcaster.subscribe_instance(&p.instance_id, p.include_ctx);

        session.add_instance_subscription(subscription_id.clone(), p.instance_id.clone());

        // Update subscription metrics
        if let Some(ref metrics) = self.metrics {
            metrics
                .subscriptions_active
                .with_label_values(&["instance"])
                .inc();
        }

        let result = WatchInstanceResult {
            subscription_id,
            instance_id: p.instance_id,
            current_state: instance.state,
            current_wal_offset: instance.last_wal_offset,
        };

        Ok((serde_json::to_value(result)?, receiver))
    }

    /// Internal handler for WATCH_ALL that returns the value only.
    fn handle_watch_all_cmd(
        &self,
        session: &mut Session,
        params: &Value,
    ) -> Result<Value, ServerError> {
        let p: WatchAllParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        let broadcaster = self.broadcaster.as_ref().ok_or_else(|| {
            ServerError::InvalidRequest("streaming not enabled on this server".to_string())
        })?;

        let filter = EventFilter {
            machines: p.machines,
            from_states: p.from_states,
            to_states: p.to_states,
            events: p.events,
        };

        let (subscription_id, _receiver) = broadcaster.subscribe_all(filter, p.include_ctx);

        session.add_all_subscription(subscription_id.clone());

        // Get current WAL head offset
        let wal_offset = self
            .engine
            .wal()
            .latest_offset()
            .map(|o| o.as_u64())
            .unwrap_or(0);

        let result = WatchAllResult {
            subscription_id,
            wal_offset,
        };

        Ok(serde_json::to_value(result)?)
    }

    /// Handles WATCH_ALL and returns the receiver for streaming.
    /// This is called by the server to get both the response and the receiver.
    pub fn handle_watch_all(
        &self,
        session: &mut Session,
        params: &Value,
    ) -> Result<(Value, broadcast::Receiver<InstanceEvent>, EventFilter), ServerError> {
        let p: WatchAllParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        let broadcaster = self.broadcaster.as_ref().ok_or_else(|| {
            ServerError::InvalidRequest("streaming not enabled on this server".to_string())
        })?;

        let filter = EventFilter {
            machines: p.machines,
            from_states: p.from_states,
            to_states: p.to_states,
            events: p.events,
        };

        let (subscription_id, receiver) = broadcaster.subscribe_all(filter.clone(), p.include_ctx);

        session.add_all_subscription(subscription_id.clone());

        // Update subscription metrics
        if let Some(ref metrics) = self.metrics {
            metrics
                .subscriptions_active
                .with_label_values(&["all"])
                .inc();
        }

        // Get current WAL head offset
        let wal_offset = self
            .engine
            .wal()
            .latest_offset()
            .map(|o| o.as_u64())
            .unwrap_or(0);

        let result = WatchAllResult {
            subscription_id,
            wal_offset,
        };

        Ok((serde_json::to_value(result)?, receiver, filter))
    }

    fn handle_unwatch(&self, session: &mut Session, params: &Value) -> Result<Value, ServerError> {
        let p: UnwatchParams = serde_json::from_value(params.clone())
            .map_err(|e| ServerError::InvalidRequest(e.to_string()))?;

        // Remove from session and get type for metrics
        let removed_type = session.remove_subscription(&p.subscription_id);

        // Update subscription metrics
        if let Some(ref metrics) = self.metrics {
            match &removed_type {
                Some(crate::session::SessionSubscriptionType::Instance { .. }) => {
                    metrics
                        .subscriptions_active
                        .with_label_values(&["instance"])
                        .dec();
                }
                Some(crate::session::SessionSubscriptionType::All) => {
                    metrics
                        .subscriptions_active
                        .with_label_values(&["all"])
                        .dec();
                }
                None => {}
            }
        }

        // Also remove from broadcaster if present
        if let Some(broadcaster) = &self.broadcaster {
            broadcaster.unsubscribe(&p.subscription_id);
        }

        let result = UnwatchResult {
            subscription_id: p.subscription_id,
            removed: removed_type.is_some(),
        };

        Ok(serde_json::to_value(result)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::broadcast::EventBroadcaster;
    use rstmdb_wal::{FsyncPolicy, WalConfig};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use tempfile::TempDir;

    fn test_handler() -> (TempDir, CommandHandler, Session) {
        let dir = TempDir::new().unwrap();
        let config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        let engine = Arc::new(StateMachineEngine::new(config).unwrap());
        let handler = CommandHandler::new(engine);
        let session = Session::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            false,
        );
        (dir, handler, session)
    }

    fn test_handler_with_broadcaster() -> (TempDir, CommandHandler, Session) {
        let dir = TempDir::new().unwrap();
        let config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        let engine = Arc::new(StateMachineEngine::new(config).unwrap());
        let broadcaster = Arc::new(EventBroadcaster::new(16));
        let handler = CommandHandler::new(engine).with_broadcaster(broadcaster);
        let session = Session::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            false,
        );
        (dir, handler, session)
    }

    #[test]
    fn test_hello() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::Hello).with_params(json!({
            "protocol_version": 1,
            "client_name": "test",
            "wire_modes": ["binary_json"],
            "features": ["idempotency"]
        }));

        let response = handler.handle(&mut session, &request);
        assert!(response.is_ok());
        assert_eq!(session.state(), SessionState::Authenticated);
    }

    #[test]
    fn test_ping_pong() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::Ping);
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        assert_eq!(response.result.unwrap()["pong"], true);
    }

    #[test]
    fn test_put_and_get_machine() {
        let (_dir, handler, mut session) = test_handler();

        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "order",
            "version": 1,
            "definition": {
                "states": ["created", "paid"],
                "initial": "created",
                "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
            }
        }));

        let response = handler.handle(&mut session, &put_request);
        assert!(response.is_ok());

        let get_request = Request::new("2", Operation::GetMachine).with_params(json!({
            "machine": "order",
            "version": 1
        }));

        let response = handler.handle(&mut session, &get_request);
        assert!(response.is_ok());
    }

    #[test]
    fn test_create_and_apply() {
        let (_dir, handler, mut session) = test_handler();

        // Put machine
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "order",
            "version": 1,
            "definition": {
                "states": ["created", "paid"],
                "initial": "created",
                "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
            }
        }));
        handler.handle(&mut session, &put_request);

        // Create instance
        let create_request = Request::new("2", Operation::CreateInstance).with_params(json!({
            "instance_id": "i-1",
            "machine": "order",
            "version": 1
        }));
        let response = handler.handle(&mut session, &create_request);
        assert!(response.is_ok());

        // Apply event
        let apply_request = Request::new("3", Operation::ApplyEvent).with_params(json!({
            "instance_id": "i-1",
            "event": "PAY"
        }));
        let response = handler.handle(&mut session, &apply_request);
        assert!(response.is_ok());

        let result = response.result.unwrap();
        assert_eq!(result["from_state"], "created");
        assert_eq!(result["to_state"], "paid");
    }

    #[test]
    fn test_info() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::Info);
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert_eq!(result["server_name"], "rstmdb");
        assert!(result["features"].as_array().is_some());
    }

    #[test]
    fn test_bye() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::Bye);
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        assert_eq!(session.state(), SessionState::Closing);
    }

    #[test]
    fn test_auth_success() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::Auth).with_params(json!({
            "method": "bearer",
            "token": "secret-token"
        }));
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        assert!(session.is_authenticated());
    }

    #[test]
    fn test_auth_failure_empty_token() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::Auth).with_params(json!({
            "method": "bearer",
            "token": ""
        }));
        let response = handler.handle(&mut session, &request);

        assert!(response.is_error());
    }

    #[test]
    fn test_list_machines() {
        let (_dir, handler, mut session) = test_handler();

        // Put a machine first
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "workflow",
            "version": 1,
            "definition": {
                "states": ["pending", "done"],
                "initial": "pending",
                "transitions": [{"from": "pending", "event": "COMPLETE", "to": "done"}]
            }
        }));
        handler.handle(&mut session, &put_request);

        let request = Request::new("2", Operation::ListMachines);
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        let result = response.result.unwrap();
        let items = result["items"].as_array().unwrap();
        assert!(!items.is_empty());
    }

    #[test]
    fn test_list_machines_sorted_alphabetically() {
        let (_dir, handler, mut session) = test_handler();

        // Create machines in non-alphabetical order
        let machines = ["zebra", "apple", "mango", "banana"];
        for name in machines {
            let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
                "machine": name,
                "version": 1,
                "definition": {
                    "states": ["pending", "done"],
                    "initial": "pending",
                    "transitions": [{"from": "pending", "event": "COMPLETE", "to": "done"}]
                }
            }));
            handler.handle(&mut session, &put_request);
        }

        // List machines
        let request = Request::new("2", Operation::ListMachines);
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        let result = response.result.unwrap();
        let items = result["items"].as_array().unwrap();

        // Verify they are sorted alphabetically
        let names: Vec<&str> = items
            .iter()
            .map(|item| item["machine"].as_str().unwrap())
            .collect();

        assert_eq!(names, vec!["apple", "banana", "mango", "zebra"]);
    }

    #[test]
    fn test_get_machine_not_found() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::GetMachine).with_params(json!({
            "machine": "nonexistent",
            "version": 1
        }));
        let response = handler.handle(&mut session, &request);

        assert!(response.is_error());
    }

    #[test]
    fn test_get_instance_not_found() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::GetInstance).with_params(json!({
            "instance_id": "nonexistent"
        }));
        let response = handler.handle(&mut session, &request);

        assert!(response.is_error());
    }

    #[test]
    fn test_list_instances() {
        let (_dir, handler, mut session) = test_handler();

        // Setup: put machine
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "order",
            "version": 1,
            "definition": {
                "states": ["created", "paid"],
                "initial": "created",
                "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
            }
        }));
        handler.handle(&mut session, &put_request);

        // Create multiple instances
        for i in 1..=5 {
            let create_request = Request::new(format!("{}", i + 1), Operation::CreateInstance)
                .with_params(json!({
                    "instance_id": format!("order-{}", i),
                    "machine": "order",
                    "version": 1
                }));
            handler.handle(&mut session, &create_request);
        }

        // Apply event to some instances to change their state
        for i in 1..=3 {
            let apply_request = Request::new(format!("{}", i + 10), Operation::ApplyEvent)
                .with_params(json!({
                    "instance_id": format!("order-{}", i),
                    "event": "PAY"
                }));
            handler.handle(&mut session, &apply_request);
        }

        // List all instances
        let list_request = Request::new("20", Operation::ListInstances);
        let response = handler.handle(&mut session, &list_request);
        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert_eq!(result["total"], 5);
        assert_eq!(result["instances"].as_array().unwrap().len(), 5);

        // Filter by state = "paid"
        let list_request = Request::new("21", Operation::ListInstances).with_params(json!({
            "state": "paid"
        }));
        let response = handler.handle(&mut session, &list_request);
        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert_eq!(result["total"], 3);

        // Filter by state = "created"
        let list_request = Request::new("22", Operation::ListInstances).with_params(json!({
            "state": "created"
        }));
        let response = handler.handle(&mut session, &list_request);
        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert_eq!(result["total"], 2);

        // Test pagination
        let list_request = Request::new("23", Operation::ListInstances).with_params(json!({
            "limit": 2
        }));
        let response = handler.handle(&mut session, &list_request);
        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert_eq!(result["total"], 5);
        assert_eq!(result["instances"].as_array().unwrap().len(), 2);
        assert_eq!(result["has_more"], true);

        // Test offset
        let list_request = Request::new("24", Operation::ListInstances).with_params(json!({
            "limit": 2,
            "offset": 4
        }));
        let response = handler.handle(&mut session, &list_request);
        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert_eq!(result["instances"].as_array().unwrap().len(), 1);
        assert_eq!(result["has_more"], false);
    }

    #[test]
    fn test_delete_instance() {
        let (_dir, handler, mut session) = test_handler();

        // Setup: put machine and create instance
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "order",
            "version": 1,
            "definition": {
                "states": ["created", "paid"],
                "initial": "created",
                "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
            }
        }));
        handler.handle(&mut session, &put_request);

        let create_request = Request::new("2", Operation::CreateInstance).with_params(json!({
            "instance_id": "i-delete-test",
            "machine": "order",
            "version": 1
        }));
        handler.handle(&mut session, &create_request);

        // Delete instance
        let delete_request = Request::new("3", Operation::DeleteInstance).with_params(json!({
            "instance_id": "i-delete-test"
        }));
        let response = handler.handle(&mut session, &delete_request);

        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert_eq!(result["deleted"], true);
    }

    #[test]
    fn test_batch_best_effort() {
        let (_dir, handler, mut session) = test_handler();

        // Setup: put machine
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "batch_test",
            "version": 1,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        handler.handle(&mut session, &put_request);

        let batch_request = Request::new("2", Operation::Batch).with_params(json!({
            "mode": "best_effort",
            "ops": [
                {"op": "CREATE_INSTANCE", "params": {"instance_id": "b1", "machine": "batch_test", "version": 1}},
                {"op": "CREATE_INSTANCE", "params": {"instance_id": "b2", "machine": "batch_test", "version": 1}}
            ]
        }));
        let response = handler.handle(&mut session, &batch_request);

        assert!(response.is_ok());
        let result = response.result.unwrap();
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0]["status"], "ok");
        assert_eq!(results[1]["status"], "ok");
    }

    #[test]
    fn test_batch_atomic_failure() {
        let (_dir, handler, mut session) = test_handler();

        // Batch with an invalid operation should fail atomically
        let batch_request = Request::new("1", Operation::Batch).with_params(json!({
            "mode": "atomic",
            "ops": [
                {"op": "GET_MACHINE", "params": {"machine": "nonexistent", "version": 1}}
            ]
        }));
        let response = handler.handle(&mut session, &batch_request);

        assert!(response.is_error());
    }

    #[test]
    fn test_batch_exceeds_limit() {
        let (_dir, handler, mut session) = test_handler();

        // Create 101 ops (exceeds default limit of 100)
        let ops: Vec<_> = (0..101)
            .map(|_| json!({"op": "PING", "params": {}}))
            .collect();

        let batch_request = Request::new("1", Operation::Batch).with_params(json!({
            "mode": "best_effort",
            "ops": ops
        }));
        let response = handler.handle(&mut session, &batch_request);

        assert!(response.is_error());
    }

    #[test]
    fn test_snapshot_instance() {
        let (_dir, handler, mut session) = test_handler();

        // Setup
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "snap",
            "version": 1,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        handler.handle(&mut session, &put_request);

        let create_request = Request::new("2", Operation::CreateInstance).with_params(json!({
            "instance_id": "i-snap",
            "machine": "snap",
            "version": 1
        }));
        handler.handle(&mut session, &create_request);

        let request = Request::new("3", Operation::SnapshotInstance).with_params(json!({
            "instance_id": "i-snap"
        }));
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert!(result["snapshot_id"].as_str().unwrap().starts_with("snap-"));
    }

    #[test]
    fn test_wal_read() {
        let (_dir, handler, mut session) = test_handler();

        // Setup: create some WAL entries
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "wal_test",
            "version": 1,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        handler.handle(&mut session, &put_request);

        let request = Request::new("2", Operation::WalRead).with_params(json!({
            "from_offset": 0,
            "limit": 10
        }));
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert!(result["records"].as_array().is_some());
    }

    #[test]
    fn test_wal_stats() {
        let (_dir, handler, mut session) = test_handler();

        // Setup: create some WAL entries
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "stats_test",
            "version": 1,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        handler.handle(&mut session, &put_request);

        let request = Request::new("2", Operation::WalStats);
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert!(result["entry_count"].as_u64().unwrap() >= 1);
        assert!(result["segment_count"].as_u64().is_some());
        assert!(result["total_size_bytes"].as_u64().is_some());
        assert!(result["io_stats"]["writes"].as_u64().is_some());
        assert!(result["io_stats"]["fsyncs"].as_u64().is_some());
    }

    #[test]
    fn test_watch_and_unwatch() {
        let (_dir, handler, mut session) = test_handler_with_broadcaster();

        // Setup
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "watch_test",
            "version": 1,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        handler.handle(&mut session, &put_request);

        let create_request = Request::new("2", Operation::CreateInstance).with_params(json!({
            "instance_id": "i-watch",
            "machine": "watch_test",
            "version": 1
        }));
        handler.handle(&mut session, &create_request);

        // Watch
        let watch_request = Request::new("3", Operation::WatchInstance).with_params(json!({
            "instance_id": "i-watch"
        }));
        let response = handler.handle(&mut session, &watch_request);
        assert!(response.is_ok());
        let result = response.result.unwrap();
        let subscription_id = result["subscription_id"].as_str().unwrap().to_string();
        assert_eq!(result["instance_id"], "i-watch");
        assert_eq!(result["current_state"], "init");

        // Unwatch
        let unwatch_request = Request::new("4", Operation::Unwatch).with_params(json!({
            "subscription_id": subscription_id
        }));
        let response = handler.handle(&mut session, &unwatch_request);
        assert!(response.is_ok());
        assert_eq!(response.result.unwrap()["removed"], true);
    }

    #[test]
    fn test_watch_all() {
        let (_dir, handler, mut session) = test_handler_with_broadcaster();

        // Watch all
        let watch_request = Request::new("1", Operation::WatchAll).with_params(json!({
            "machines": ["order"],
            "to_states": ["shipped"]
        }));
        let response = handler.handle(&mut session, &watch_request);
        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert!(result["subscription_id"]
            .as_str()
            .unwrap()
            .starts_with("sub-"));
        assert!(result["wal_offset"].as_u64().is_some());
    }

    #[test]
    fn test_watch_without_broadcaster_fails() {
        let (_dir, handler, mut session) = test_handler();

        // Setup
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "test",
            "version": 1,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        handler.handle(&mut session, &put_request);

        let create_request = Request::new("2", Operation::CreateInstance).with_params(json!({
            "instance_id": "i-test",
            "machine": "test",
            "version": 1
        }));
        handler.handle(&mut session, &create_request);

        // Watch should fail without broadcaster
        let watch_request = Request::new("3", Operation::WatchInstance).with_params(json!({
            "instance_id": "i-test"
        }));
        let response = handler.handle(&mut session, &watch_request);
        assert!(response.is_error());
    }

    #[test]
    fn test_apply_event_with_expected_state() {
        let (_dir, handler, mut session) = test_handler();

        // Setup
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "order",
            "version": 1,
            "definition": {
                "states": ["created", "paid", "shipped"],
                "initial": "created",
                "transitions": [
                    {"from": "created", "event": "PAY", "to": "paid"},
                    {"from": "paid", "event": "SHIP", "to": "shipped"}
                ]
            }
        }));
        handler.handle(&mut session, &put_request);

        let create_request = Request::new("2", Operation::CreateInstance).with_params(json!({
            "instance_id": "i-expected",
            "machine": "order",
            "version": 1
        }));
        handler.handle(&mut session, &create_request);

        // Apply with correct expected_state
        let apply_request = Request::new("3", Operation::ApplyEvent).with_params(json!({
            "instance_id": "i-expected",
            "event": "PAY",
            "expected_state": "created"
        }));
        let response = handler.handle(&mut session, &apply_request);
        assert!(response.is_ok());

        // Apply with wrong expected_state (should fail)
        let apply_request = Request::new("4", Operation::ApplyEvent).with_params(json!({
            "instance_id": "i-expected",
            "event": "SHIP",
            "expected_state": "created"  // wrong, it's "paid" now
        }));
        let response = handler.handle(&mut session, &apply_request);
        assert!(response.is_error());
    }

    #[test]
    fn test_max_machine_versions_limit() {
        let dir = TempDir::new().unwrap();
        let config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        let engine = Arc::new(StateMachineEngine::new(config).unwrap());
        let handler = CommandHandler::new(engine).with_max_machine_versions(2);
        let mut session = Session::new(
            SocketAddr::new(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)), 12345),
            false,
        );

        // Create version 1
        let request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "limited",
            "version": 1,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        let response = handler.handle(&mut session, &request);
        assert!(response.is_ok());

        // Create version 2
        let request = Request::new("2", Operation::PutMachine).with_params(json!({
            "machine": "limited",
            "version": 2,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        let response = handler.handle(&mut session, &request);
        assert!(response.is_ok());

        // Create version 3 should fail (limit is 2)
        let request = Request::new("3", Operation::PutMachine).with_params(json!({
            "machine": "limited",
            "version": 3,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        let response = handler.handle(&mut session, &request);
        assert!(response.is_error());

        // But re-submitting existing version 1 with same definition should work (idempotent)
        let request = Request::new("4", Operation::PutMachine).with_params(json!({
            "machine": "limited",
            "version": 1,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        let response = handler.handle(&mut session, &request);
        assert!(response.is_ok());
        // Check that created=false (idempotent)
        let result = response.result.unwrap();
        assert_eq!(result["created"], false);
    }

    #[test]
    fn test_invalid_transition() {
        let (_dir, handler, mut session) = test_handler();

        // Setup
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "order",
            "version": 1,
            "definition": {
                "states": ["created", "paid"],
                "initial": "created",
                "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
            }
        }));
        handler.handle(&mut session, &put_request);

        let create_request = Request::new("2", Operation::CreateInstance).with_params(json!({
            "instance_id": "i-invalid",
            "machine": "order",
            "version": 1
        }));
        handler.handle(&mut session, &create_request);

        // Apply invalid event
        let apply_request = Request::new("3", Operation::ApplyEvent).with_params(json!({
            "instance_id": "i-invalid",
            "event": "SHIP"  // not valid from "created" state
        }));
        let response = handler.handle(&mut session, &apply_request);
        assert!(response.is_error());
    }

    #[test]
    fn test_hello_unsupported_protocol_version() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::Hello).with_params(json!({
            "protocol_version": 999,
            "client_name": "test",
            "wire_modes": ["binary_json"],
            "features": []
        }));
        let response = handler.handle(&mut session, &request);

        assert!(response.is_error());
    }

    #[test]
    fn test_hello_jsonl_wire_mode() {
        let (_dir, handler, mut session) = test_handler();

        let request = Request::new("1", Operation::Hello).with_params(json!({
            "protocol_version": 1,
            "client_name": "test",
            "wire_modes": ["jsonl"],
            "features": []
        }));
        let response = handler.handle(&mut session, &request);

        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert_eq!(result["wire_mode"], "jsonl");
    }

    #[test]
    fn test_create_instance_with_initial_ctx() {
        let (_dir, handler, mut session) = test_handler();

        // Setup
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "order",
            "version": 1,
            "definition": {
                "states": ["created"],
                "initial": "created",
                "transitions": []
            }
        }));
        handler.handle(&mut session, &put_request);

        let create_request = Request::new("2", Operation::CreateInstance).with_params(json!({
            "instance_id": "i-ctx",
            "machine": "order",
            "version": 1,
            "initial_ctx": {"amount": 100, "user": "alice"}
        }));
        let response = handler.handle(&mut session, &create_request);
        assert!(response.is_ok());

        // Get and verify ctx
        let get_request = Request::new("3", Operation::GetInstance).with_params(json!({
            "instance_id": "i-ctx"
        }));
        let response = handler.handle(&mut session, &get_request);
        assert!(response.is_ok());
        let result = response.result.unwrap();
        assert_eq!(result["ctx"]["amount"], 100);
        assert_eq!(result["ctx"]["user"], "alice");
    }

    #[test]
    fn test_create_instance_auto_id() {
        let (_dir, handler, mut session) = test_handler();

        // Setup
        let put_request = Request::new("1", Operation::PutMachine).with_params(json!({
            "machine": "auto",
            "version": 1,
            "definition": {
                "states": ["init"],
                "initial": "init",
                "transitions": []
            }
        }));
        handler.handle(&mut session, &put_request);

        // Create without instance_id
        let create_request = Request::new("2", Operation::CreateInstance).with_params(json!({
            "machine": "auto",
            "version": 1
        }));
        let response = handler.handle(&mut session, &create_request);
        assert!(response.is_ok());

        let result = response.result.unwrap();
        let instance_id = result["instance_id"].as_str().unwrap();
        assert!(!instance_id.is_empty());
    }
}
