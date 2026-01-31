//! # rstmdb-server
//!
//! TCP server for rstmdb.
//!
//! This crate provides:
//! - TCP connection handling with async I/O
//! - Protocol framing and message dispatch
//! - Session management
//! - Command handlers for all RCP operations
//! - Token-based authentication
//! - Automatic compaction
//! - Optional TLS support

pub mod auth;
pub mod broadcast;
pub mod compaction;
pub mod config;
pub mod error;
pub mod handler;
pub mod server;
pub mod session;
pub mod stream;
pub mod tls;

pub use auth::TokenValidator;
pub use broadcast::{EventBroadcaster, EventFilter, InstanceEvent, Subscription, SubscriptionType};
pub use compaction::CompactionManager;
pub use config::{AuthConfig, CompactionConfig, Config, NetworkConfig, StorageConfig, TlsConfig};
pub use error::ServerError;
pub use handler::CommandHandler;
pub use server::{Server, ServerConfig};
pub use session::Session;
