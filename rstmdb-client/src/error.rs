//! Client error types.

use rstmdb_protocol::ErrorCode;
use thiserror::Error;

/// Client errors.
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("protocol error: {0}")]
    Protocol(#[from] rstmdb_protocol::ProtocolError),

    #[error("not connected")]
    NotConnected,

    #[error("connection closed")]
    ConnectionClosed,

    #[error("request timeout")]
    Timeout,

    #[error("server error: {code} - {message}")]
    ServerError {
        code: ErrorCode,
        message: String,
        retryable: bool,
    },

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("TLS configuration error: {0}")]
    TlsConfig(String),

    #[error("TLS handshake failed: {0}")]
    TlsHandshake(String),
}

impl ClientError {
    /// Returns whether this error is retryable.
    pub fn is_retryable(&self) -> bool {
        match self {
            ClientError::Io(_) => true,
            ClientError::Timeout => true,
            ClientError::ConnectionClosed => true,
            ClientError::ServerError { retryable, .. } => *retryable,
            _ => false,
        }
    }
}
