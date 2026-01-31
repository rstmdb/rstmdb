//! # rstmdb-client
//!
//! Client library for rstmdb.
//!
//! This crate provides:
//! - Async TCP client with connection management
//! - High-level API for all RCP operations
//! - Automatic reconnection and retry logic
//! - Optional TLS support

pub mod client;
pub mod connection;
pub mod error;
pub mod stream;
pub mod tls;

pub use client::Client;
pub use connection::{Connection, ConnectionConfig, TlsClientConfig};
pub use error::ClientError;
