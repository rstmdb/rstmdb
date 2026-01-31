//! Core error types.

use thiserror::Error;

/// Errors from the state machine engine.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("machine not found: {machine}")]
    MachineNotFound { machine: String },

    #[error("machine version not found: {machine} v{version}")]
    MachineVersionNotFound { machine: String, version: u32 },

    #[error("machine version already exists: {machine} v{version}")]
    MachineVersionExists { machine: String, version: u32 },

    #[error("instance not found: {instance_id}")]
    InstanceNotFound { instance_id: String },

    #[error("instance already exists: {instance_id}")]
    InstanceExists { instance_id: String },

    #[error("invalid transition: cannot apply '{event}' in state '{state}'")]
    InvalidTransition { state: String, event: String },

    #[error("guard failed: {reason}")]
    GuardFailed { reason: String },

    #[error("state conflict: expected '{expected}', actual '{actual}'")]
    StateConflict { expected: String, actual: String },

    #[error("WAL offset conflict: expected {expected}, actual {actual}")]
    WalOffsetConflict { expected: u64, actual: u64 },

    #[error("invalid machine definition: {reason}")]
    InvalidDefinition { reason: String },

    #[error("invalid guard expression: {reason}")]
    InvalidGuard { reason: String },

    #[error("WAL error: {0}")]
    Wal(#[from] rstmdb_wal::WalError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

impl CoreError {
    /// Returns whether this error indicates the operation can be retried.
    pub fn is_retryable(&self) -> bool {
        matches!(self, CoreError::Wal(e) if e.is_retryable())
    }

    /// Returns an error code suitable for protocol responses.
    pub fn error_code(&self) -> &'static str {
        match self {
            CoreError::MachineNotFound { .. } => "MACHINE_NOT_FOUND",
            CoreError::MachineVersionNotFound { .. } => "MACHINE_NOT_FOUND",
            CoreError::MachineVersionExists { .. } => "MACHINE_VERSION_EXISTS",
            CoreError::InstanceNotFound { .. } => "INSTANCE_NOT_FOUND",
            CoreError::InstanceExists { .. } => "INSTANCE_EXISTS",
            CoreError::InvalidTransition { .. } => "INVALID_TRANSITION",
            CoreError::GuardFailed { .. } => "GUARD_FAILED",
            CoreError::StateConflict { .. } => "CONFLICT",
            CoreError::WalOffsetConflict { .. } => "CONFLICT",
            CoreError::InvalidDefinition { .. } => "BAD_REQUEST",
            CoreError::InvalidGuard { .. } => "BAD_REQUEST",
            CoreError::Wal(_) => "WAL_IO_ERROR",
            CoreError::Json(_) => "BAD_REQUEST",
        }
    }
}
