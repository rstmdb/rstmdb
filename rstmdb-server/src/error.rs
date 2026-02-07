//! Server error types.

use rstmdb_protocol::ErrorCode;
use thiserror::Error;

/// Server errors.
#[derive(Debug, Error)]
pub enum ServerError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("protocol error: {0}")]
    Protocol(#[from] rstmdb_protocol::ProtocolError),

    #[error("core error: {0}")]
    Core(#[from] rstmdb_core::CoreError),

    #[error("storage error: {0}")]
    Storage(#[from] rstmdb_storage::StorageError),

    #[error("WAL error: {0}")]
    Wal(#[from] rstmdb_wal::WalError),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("session not authenticated")]
    NotAuthenticated,

    #[error("authentication failed: {0}")]
    AuthFailed(String),

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("machine version limit exceeded: {0}")]
    MachineVersionLimitExceeded(String),

    #[error("server shutting down")]
    ShuttingDown,

    #[error("TLS configuration error: {0}")]
    TlsConfig(String),

    #[error("TLS handshake failed: {0}")]
    TlsHandshake(String),
}

impl ServerError {
    /// Converts to protocol error code.
    pub fn error_code(&self) -> ErrorCode {
        match self {
            ServerError::Io(_) => ErrorCode::InternalError,
            ServerError::Protocol(_) => ErrorCode::BadRequest,
            ServerError::Core(e) => match e.error_code() {
                "MACHINE_NOT_FOUND" => ErrorCode::MachineNotFound,
                "MACHINE_VERSION_EXISTS" => ErrorCode::MachineVersionExists,
                "INSTANCE_NOT_FOUND" => ErrorCode::InstanceNotFound,
                "INSTANCE_EXISTS" => ErrorCode::InstanceExists,
                "INVALID_TRANSITION" => ErrorCode::InvalidTransition,
                "GUARD_FAILED" => ErrorCode::GuardFailed,
                "CONFLICT" => ErrorCode::Conflict,
                "WAL_IO_ERROR" => ErrorCode::WalIoError,
                _ => ErrorCode::InternalError,
            },
            ServerError::Storage(_) => ErrorCode::InternalError,
            ServerError::Wal(_) => ErrorCode::WalIoError,
            ServerError::Json(_) => ErrorCode::BadRequest,
            ServerError::NotAuthenticated => ErrorCode::Unauthorized,
            ServerError::AuthFailed(_) => ErrorCode::AuthFailed,
            ServerError::InvalidRequest(_) => ErrorCode::BadRequest,
            ServerError::MachineVersionLimitExceeded(_) => ErrorCode::MachineVersionLimitExceeded,
            ServerError::ShuttingDown => ErrorCode::InternalError,
            ServerError::TlsConfig(_) => ErrorCode::InternalError,
            ServerError::TlsHandshake(_) => ErrorCode::InternalError,
        }
    }

    /// Returns whether this error is retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.error_code(),
            ErrorCode::WalIoError | ErrorCode::RateLimited | ErrorCode::InternalError
        )
    }
}
