//! # rstmdb-protocol
//!
//! Wire protocol implementation for rstmdb (RCP - rstmdb Command Protocol).
//!
//! This crate provides:
//! - Binary framing with length prefix and CRC32C validation
//! - JSON message serialization/deserialization
//! - Request/Response envelope types
//! - Error codes and protocol constants

pub mod codec;
pub mod error;
pub mod frame;
pub mod message;

pub use codec::{Decoder, Encoder};
pub use error::{ErrorCode, ProtocolError};
pub use frame::{Frame, FrameFlags, FRAME_HEADER_SIZE, MAGIC};
pub use message::{Operation, Request, Response, ResponseError, ResponseMeta, ResponseStatus};

/// Protocol version supported by this implementation.
pub const PROTOCOL_VERSION: u16 = 1;

/// Default port for rstmdb server.
pub const DEFAULT_PORT: u16 = 7401;

/// Maximum frame payload size (16 MiB).
pub const MAX_PAYLOAD_SIZE: u32 = 16 * 1024 * 1024;
