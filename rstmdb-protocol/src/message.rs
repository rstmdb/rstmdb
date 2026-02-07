//! JSON message types for RCP requests and responses.

use crate::error::ErrorCode;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

/// RCP operation types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Operation {
    // Session management
    Hello,
    Auth,
    Ping,
    Bye,

    // Server info
    Info,

    // Machine definition management
    PutMachine,
    GetMachine,
    ListMachines,

    // Instance lifecycle
    CreateInstance,
    GetInstance,
    ListInstances,
    DeleteInstance,

    // Events
    ApplyEvent,
    Batch,

    // Snapshots and WAL
    SnapshotInstance,
    WalRead,
    WalStats,
    Compact,

    // Subscriptions
    WatchInstance,
    WatchAll,
    Unwatch,
}

/// Request message envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    /// Message type, always "request".
    #[serde(rename = "type")]
    pub msg_type: String,

    /// Unique request ID for correlation.
    pub id: String,

    /// Operation to perform.
    pub op: Operation,

    /// Operation-specific parameters.
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(id: impl Into<String>, op: Operation) -> Self {
        Self {
            msg_type: "request".to_string(),
            id: id.into(),
            op,
            params: Value::Object(Default::default()),
        }
    }

    pub fn with_params(mut self, params: Value) -> Self {
        self.params = params;
        self
    }
}

/// Response status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseStatus {
    Ok,
    Error,
}

/// Error details in a response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseError {
    /// Stable error code.
    pub code: ErrorCode,

    /// Human-readable error message.
    pub message: String,

    /// Whether this error is retryable.
    pub retryable: bool,

    /// Additional error details.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub details: HashMap<String, Value>,
}

impl ResponseError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            retryable: code.is_retryable(),
            code,
            message: message.into(),
            details: HashMap::new(),
        }
    }

    pub fn with_detail(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.details.insert(key.into(), value.into());
        self
    }
}

/// Response metadata.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ResponseMeta {
    /// Server timestamp.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_time: Option<DateTime<Utc>>,

    /// Whether this server is the leader (for cluster mode).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub leader: Option<bool>,

    /// WAL offset after write operations.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wal_offset: Option<u64>,

    /// Trace ID for distributed tracing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,

    /// Additional metadata fields (for forward compatibility).
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Response message envelope.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    /// Message type, always "response".
    #[serde(rename = "type")]
    pub msg_type: String,

    /// Request ID this response correlates to.
    pub id: String,

    /// Response status.
    pub status: ResponseStatus,

    /// Result payload (for successful responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,

    /// Error details (for error responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,

    /// Response metadata.
    #[serde(default, skip_serializing_if = "is_meta_empty")]
    pub meta: ResponseMeta,
}

fn is_meta_empty(meta: &ResponseMeta) -> bool {
    meta.server_time.is_none()
        && meta.leader.is_none()
        && meta.wal_offset.is_none()
        && meta.trace_id.is_none()
        && meta.extra.is_empty()
}

impl Response {
    pub fn ok(id: impl Into<String>, result: Value) -> Self {
        Self {
            msg_type: "response".to_string(),
            id: id.into(),
            status: ResponseStatus::Ok,
            result: Some(result),
            error: None,
            meta: ResponseMeta::default(),
        }
    }

    pub fn error(id: impl Into<String>, error: ResponseError) -> Self {
        Self {
            msg_type: "response".to_string(),
            id: id.into(),
            status: ResponseStatus::Error,
            result: None,
            error: Some(error),
            meta: ResponseMeta::default(),
        }
    }

    pub fn with_meta(mut self, meta: ResponseMeta) -> Self {
        self.meta = meta;
        self
    }

    pub fn is_ok(&self) -> bool {
        self.status == ResponseStatus::Ok
    }

    pub fn is_error(&self) -> bool {
        self.status == ResponseStatus::Error
    }
}

/// Streaming event message (for WATCH_INSTANCE, WATCH_ALL, etc.).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamEvent {
    /// Message type, always "event".
    #[serde(rename = "type")]
    pub msg_type: String,

    /// Subscription ID.
    pub subscription_id: String,

    /// Instance ID.
    pub instance_id: String,

    /// Machine name.
    pub machine: String,

    /// Machine version.
    pub version: u32,

    /// WAL offset of this event.
    pub wal_offset: u64,

    /// State before transition.
    pub from_state: String,

    /// State after transition.
    pub to_state: String,

    /// Event that triggered the transition.
    pub event: String,

    /// Event payload (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payload: Option<Value>,

    /// Instance context (optional, if requested).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx: Option<Value>,
}

// ============================================================================
// Operation-specific parameter types
// ============================================================================

/// Parameters for HELLO request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloParams {
    pub protocol_version: u16,
    #[serde(default)]
    pub client_name: Option<String>,
    #[serde(default)]
    pub wire_modes: Vec<String>,
    #[serde(default)]
    pub features: Vec<String>,
}

/// Result for HELLO response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol_version: u16,
    pub wire_mode: String,
    pub server_name: String,
    pub server_version: String,
    pub features: Vec<String>,
}

/// Parameters for AUTH request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthParams {
    pub method: String,
    pub token: String,
}

/// Parameters for PUT_MACHINE request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutMachineParams {
    pub machine: String,
    pub version: u32,
    pub definition: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checksum: Option<String>,
}

/// Result for PUT_MACHINE response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PutMachineResult {
    pub machine: String,
    pub version: u32,
    pub stored_checksum: String,
    pub created: bool,
}

/// Parameters for GET_MACHINE request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMachineParams {
    pub machine: String,
    pub version: u32,
}

/// Result for GET_MACHINE response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetMachineResult {
    pub definition: Value,
    pub checksum: String,
}

/// Parameters for CREATE_INSTANCE request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInstanceParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    pub machine: String,
    pub version: u32,
    #[serde(default)]
    pub initial_ctx: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Result for CREATE_INSTANCE response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateInstanceResult {
    pub instance_id: String,
    pub state: String,
    pub wal_offset: u64,
}

/// Parameters for GET_INSTANCE request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetInstanceParams {
    pub instance_id: String,
}

/// Result for GET_INSTANCE response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GetInstanceResult {
    pub machine: String,
    pub version: u32,
    pub state: String,
    pub ctx: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_event_id: Option<String>,
    pub last_wal_offset: u64,
}

/// Parameters for LIST_INSTANCES request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListInstancesParams {
    /// Filter by machine name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub machine: Option<String>,
    /// Filter by current state.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Maximum number of instances to return.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Number of instances to skip (for pagination).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub offset: Option<u32>,
}

/// Instance summary for list responses (excludes ctx for efficiency).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstanceSummary {
    pub id: String,
    pub machine: String,
    pub version: u32,
    pub state: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub last_wal_offset: u64,
}

/// Result for LIST_INSTANCES response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ListInstancesResult {
    pub instances: Vec<InstanceSummary>,
    pub total: u64,
    pub has_more: bool,
}

/// Parameters for APPLY_EVENT request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyEventParams {
    pub instance_id: String,
    pub event: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_state: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_wal_offset: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

/// Result for APPLY_EVENT response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApplyEventResult {
    pub from_state: String,
    pub to_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ctx: Option<Value>,
    pub wal_offset: u64,
    pub applied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_id: Option<String>,
}

/// Parameters for COMPACT request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CompactParams {
    /// Force snapshot of all instances before compaction.
    #[serde(default)]
    pub force_snapshot: bool,
}

/// Result for COMPACT response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompactResult {
    /// Number of snapshots created.
    pub snapshots_created: usize,
    /// Number of WAL segments deleted.
    pub segments_deleted: usize,
    /// Bytes reclaimed from deleted segments.
    pub bytes_reclaimed: u64,
}

// ============================================================================
// Watch/Streaming parameter and result types
// ============================================================================

fn default_true() -> bool {
    true
}

/// Parameters for WATCH_INSTANCE request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchInstanceParams {
    pub instance_id: String,
    /// Include context in stream events (default: true).
    #[serde(default = "default_true")]
    pub include_ctx: bool,
    /// Start streaming from this WAL offset (optional, for replay).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_offset: Option<u64>,
}

/// Parameters for WATCH_ALL request.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WatchAllParams {
    /// Include context in stream events (default: true).
    #[serde(default = "default_true")]
    pub include_ctx: bool,
    /// Start streaming from this WAL offset (optional, for replay).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_offset: Option<u64>,
    /// Filter: only these machine types (empty = all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub machines: Vec<String>,
    /// Filter: only events FROM these states (empty = all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub from_states: Vec<String>,
    /// Filter: only events TO these states (empty = all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub to_states: Vec<String>,
    /// Filter: only these event types (empty = all).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<String>,
}

/// Result for WATCH_INSTANCE response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchInstanceResult {
    pub subscription_id: String,
    pub instance_id: String,
    pub current_state: String,
    pub current_wal_offset: u64,
}

/// Result for WATCH_ALL response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WatchAllResult {
    pub subscription_id: String,
    /// Current WAL head offset (events after this will be streamed).
    pub wal_offset: u64,
}

/// Parameters for UNWATCH request.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnwatchParams {
    pub subscription_id: String,
}

/// Result for UNWATCH response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnwatchResult {
    pub subscription_id: String,
    pub removed: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_request_serialization() {
        let req = Request::new("1", Operation::Ping);
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains(r#""op":"PING""#));
        assert!(json.contains(r#""type":"request""#));
    }

    #[test]
    fn test_response_ok_serialization() {
        let resp = Response::ok("1", serde_json::json!({"pong": true}));
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""status":"ok""#));
        assert!(json.contains(r#""pong":true"#));
    }

    #[test]
    fn test_response_error_serialization() {
        let err = ResponseError::new(ErrorCode::NotFound, "Instance not found")
            .with_detail("instance_id", "i-123");
        let resp = Response::error("1", err);
        let json = serde_json::to_string(&resp).unwrap();
        assert!(json.contains(r#""code":"NOT_FOUND""#));
        assert!(json.contains(r#""retryable":false"#));
    }
}
