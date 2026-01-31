//! Protocol error types and error codes.

use serde::{Deserialize, Serialize};
use std::fmt;
use thiserror::Error;

/// Protocol-level errors that can occur during framing or message handling.
#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("invalid magic bytes: expected 'RCPX', got {0:?}")]
    InvalidMagic([u8; 4]),

    #[error("unsupported protocol version: {0}")]
    UnsupportedVersion(u16),

    #[error("frame too large: {size} bytes (max {max})")]
    FrameTooLarge { size: u32, max: u32 },

    #[error("CRC mismatch: expected {expected:#x}, got {actual:#x}")]
    CrcMismatch { expected: u32, actual: u32 },

    #[error("invalid frame flags: {0:#x}")]
    InvalidFlags(u16),

    #[error("incomplete frame: need {needed} more bytes")]
    IncompleteFrame { needed: usize },

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid UTF-8 in payload")]
    InvalidUtf8,

    #[error("missing required field: {0}")]
    MissingField(&'static str),
}

/// Stable error codes returned in error responses.
///
/// These codes are part of the protocol contract and must remain stable
/// across versions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    // Protocol errors
    UnsupportedProtocol,
    BadRequest,

    // Authentication errors
    Unauthorized,
    AuthFailed,

    // Resource errors
    NotFound,
    MachineNotFound,
    MachineVersionExists,
    InstanceNotFound,
    InstanceExists,

    // State machine errors
    InvalidTransition,
    GuardFailed,
    Conflict,

    // System errors
    WalIoError,
    InternalError,
    RateLimited,
}

impl ErrorCode {
    /// Returns whether this error is potentially retryable.
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            ErrorCode::WalIoError | ErrorCode::RateLimited | ErrorCode::InternalError
        )
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ErrorCode::UnsupportedProtocol => write!(f, "UNSUPPORTED_PROTOCOL"),
            ErrorCode::BadRequest => write!(f, "BAD_REQUEST"),
            ErrorCode::Unauthorized => write!(f, "UNAUTHORIZED"),
            ErrorCode::AuthFailed => write!(f, "AUTH_FAILED"),
            ErrorCode::NotFound => write!(f, "NOT_FOUND"),
            ErrorCode::MachineNotFound => write!(f, "MACHINE_NOT_FOUND"),
            ErrorCode::MachineVersionExists => write!(f, "MACHINE_VERSION_EXISTS"),
            ErrorCode::InstanceNotFound => write!(f, "INSTANCE_NOT_FOUND"),
            ErrorCode::InstanceExists => write!(f, "INSTANCE_EXISTS"),
            ErrorCode::InvalidTransition => write!(f, "INVALID_TRANSITION"),
            ErrorCode::GuardFailed => write!(f, "GUARD_FAILED"),
            ErrorCode::Conflict => write!(f, "CONFLICT"),
            ErrorCode::WalIoError => write!(f, "WAL_IO_ERROR"),
            ErrorCode::InternalError => write!(f, "INTERNAL_ERROR"),
            ErrorCode::RateLimited => write!(f, "RATE_LIMITED"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_code_retryable() {
        // Retryable errors
        assert!(ErrorCode::WalIoError.is_retryable());
        assert!(ErrorCode::RateLimited.is_retryable());
        assert!(ErrorCode::InternalError.is_retryable());

        // Non-retryable errors
        assert!(!ErrorCode::BadRequest.is_retryable());
        assert!(!ErrorCode::NotFound.is_retryable());
        assert!(!ErrorCode::InvalidTransition.is_retryable());
        assert!(!ErrorCode::GuardFailed.is_retryable());
        assert!(!ErrorCode::Conflict.is_retryable());
        assert!(!ErrorCode::Unauthorized.is_retryable());
        assert!(!ErrorCode::AuthFailed.is_retryable());
    }

    #[test]
    fn test_error_code_display() {
        assert_eq!(
            format!("{}", ErrorCode::UnsupportedProtocol),
            "UNSUPPORTED_PROTOCOL"
        );
        assert_eq!(format!("{}", ErrorCode::BadRequest), "BAD_REQUEST");
        assert_eq!(format!("{}", ErrorCode::Unauthorized), "UNAUTHORIZED");
        assert_eq!(format!("{}", ErrorCode::AuthFailed), "AUTH_FAILED");
        assert_eq!(format!("{}", ErrorCode::NotFound), "NOT_FOUND");
        assert_eq!(
            format!("{}", ErrorCode::MachineNotFound),
            "MACHINE_NOT_FOUND"
        );
        assert_eq!(
            format!("{}", ErrorCode::MachineVersionExists),
            "MACHINE_VERSION_EXISTS"
        );
        assert_eq!(
            format!("{}", ErrorCode::InstanceNotFound),
            "INSTANCE_NOT_FOUND"
        );
        assert_eq!(format!("{}", ErrorCode::InstanceExists), "INSTANCE_EXISTS");
        assert_eq!(
            format!("{}", ErrorCode::InvalidTransition),
            "INVALID_TRANSITION"
        );
        assert_eq!(format!("{}", ErrorCode::GuardFailed), "GUARD_FAILED");
        assert_eq!(format!("{}", ErrorCode::Conflict), "CONFLICT");
        assert_eq!(format!("{}", ErrorCode::WalIoError), "WAL_IO_ERROR");
        assert_eq!(format!("{}", ErrorCode::InternalError), "INTERNAL_ERROR");
        assert_eq!(format!("{}", ErrorCode::RateLimited), "RATE_LIMITED");
    }

    #[test]
    fn test_error_code_serialization() {
        let code = ErrorCode::NotFound;
        let json = serde_json::to_string(&code).unwrap();
        assert_eq!(json, "\"NOT_FOUND\"");

        let parsed: ErrorCode = serde_json::from_str("\"CONFLICT\"").unwrap();
        assert_eq!(parsed, ErrorCode::Conflict);
    }

    #[test]
    fn test_protocol_error_display() {
        let err = ProtocolError::InvalidMagic(*b"XXXX");
        // InvalidMagic displays as byte array, e.g. [88, 88, 88, 88]
        assert!(err.to_string().contains("magic"));

        let err = ProtocolError::UnsupportedVersion(99);
        assert!(err.to_string().contains("99"));

        let err = ProtocolError::FrameTooLarge { size: 100, max: 50 };
        assert!(err.to_string().contains("100"));

        // CRC uses hex format
        let err = ProtocolError::CrcMismatch {
            expected: 0xABC,
            actual: 0xDEF,
        };
        let msg = err.to_string();
        assert!(msg.contains("abc") || msg.contains("ABC"));

        let err = ProtocolError::IncompleteFrame { needed: 10 };
        assert!(err.to_string().contains("10"));

        let err = ProtocolError::InvalidUtf8;
        assert!(err.to_string().contains("UTF-8"));

        let err = ProtocolError::MissingField("test_field");
        assert!(err.to_string().contains("test_field"));

        let err = ProtocolError::InvalidFlags(0xFF);
        let msg = err.to_string();
        assert!(msg.contains("ff") || msg.contains("FF"));
    }
}
