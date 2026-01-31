//! rstmdb - State Machine Database
//!
//! A TCP-based state machine database with WAL durability and snapshot compaction.

use rstmdb_core::StateMachineEngine;
use rstmdb_server::{tls, CompactionManager, Config, Server, ServerConfig};
use rstmdb_storage::SnapshotStore;
use rstmdb_wal::{FsyncPolicy, WalConfig};
use std::sync::Arc;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    // Load configuration (from file if RSTMDB_CONFIG is set, then env overrides)
    let mut config = match Config::load() {
        Ok(c) => {
            if std::env::var("RSTMDB_CONFIG").is_ok() {
                tracing::info!(
                    "Loaded config from {}",
                    std::env::var("RSTMDB_CONFIG").unwrap()
                );
            }
            c
        }
        Err(e) => {
            // If a config file was explicitly specified, fail on error
            if std::env::var("RSTMDB_CONFIG").is_ok() {
                tracing::error!("Failed to load config: {}", e);
                return Err(e.into());
            }
            // Otherwise fall back to defaults
            tracing::info!("Using default configuration");
            Config::default()
        }
    };

    // Load auth secrets from external file if configured
    if let Err(e) = config.load_secrets() {
        tracing::error!("Failed to load auth secrets: {}", e);
        return Err(e.into());
    }

    tracing::info!("Starting rstmdb server");
    tracing::info!("  Bind address: {}", config.network.bind_addr);
    tracing::info!("  Data directory: {}", config.storage.data_dir.display());

    // Log auth config
    if config.auth.required {
        if config.auth.token_hashes.is_empty() {
            tracing::error!("auth.required=true but no tokens configured!");
            return Err("Authentication required but no tokens configured".into());
        }
        tracing::info!(
            "  Authentication: enabled ({} token(s))",
            config.auth.token_hashes.len()
        );
    } else {
        tracing::info!("  Authentication: disabled");
    }

    // Validate and log TLS config
    if let Err(e) = config.tls.validate() {
        tracing::error!("TLS configuration error: {}", e);
        return Err(e.into());
    }

    let tls_acceptor = if config.tls.enabled {
        let acceptor = tls::create_tls_acceptor(&config.tls)?;
        tracing::info!("  TLS: enabled");
        if config.tls.require_client_cert {
            tracing::info!("  mTLS: enabled (client certificate required)");
        }
        Some(acceptor)
    } else {
        tracing::info!("  TLS: disabled");
        None
    };

    // Create data directories
    let wal_dir = config.storage.wal_dir();
    let snapshot_dir = config.storage.snapshots_dir();
    std::fs::create_dir_all(&wal_dir)?;
    std::fs::create_dir_all(&snapshot_dir)?;

    tracing::info!("  WAL directory: {}", wal_dir.display());
    tracing::info!("  Snapshot directory: {}", snapshot_dir.display());

    // Configure WAL
    let fsync_policy = match config.storage.fsync_policy {
        rstmdb_server::config::FsyncPolicy::EveryWrite => FsyncPolicy::EveryWrite,
        rstmdb_server::config::FsyncPolicy::EveryN(n) => FsyncPolicy::EveryN(n),
        rstmdb_server::config::FsyncPolicy::EveryMs(ms) => FsyncPolicy::EveryMs(ms),
        rstmdb_server::config::FsyncPolicy::Never => FsyncPolicy::Never,
    };
    let wal_config = WalConfig::new(&wal_dir)
        .with_segment_size(config.storage.wal_segment_size())
        .with_fsync_policy(fsync_policy);

    // Create state machine engine
    let engine = Arc::new(StateMachineEngine::new(wal_config)?);

    // Create snapshot store
    let snapshot_store = Arc::new(SnapshotStore::open(&snapshot_dir)?);

    // Configure server with snapshot, auth, and TLS support
    let mut server_config = ServerConfig::new(config.network.bind_addr);
    server_config.auth_required = config.auth.required;
    if let Some(acceptor) = tls_acceptor {
        server_config = server_config.with_tls(acceptor);
    }
    let server = Arc::new(Server::with_snapshots_and_auth(
        server_config,
        engine.clone(),
        &snapshot_dir,
        &config.auth,
    )?);

    // Create and start compaction manager
    let compaction_manager = Arc::new(CompactionManager::new(
        engine.clone(),
        snapshot_store,
        config.compaction.clone(),
    ));

    // Log compaction config
    if config.compaction.is_disabled() {
        tracing::info!("  Auto-compaction: disabled");
    } else {
        tracing::info!(
            "  Auto-compaction: enabled (events={}, size={}MB)",
            config.compaction.events_threshold,
            config.compaction.size_threshold_mb
        );
    }

    // Spawn compaction manager
    let compaction_handle = {
        let cm = compaction_manager.clone();
        tokio::spawn(async move {
            cm.run().await;
        })
    };

    // Spawn shutdown signal handler
    let shutdown_server = server.clone();
    let shutdown_compaction = compaction_manager.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("Received shutdown signal, stopping server...");
        shutdown_server.shutdown();
        shutdown_compaction.shutdown();
    });

    // Run server (blocks until shutdown)
    server.run().await?;

    // Wait for compaction manager to stop
    let _ = compaction_handle.await;

    // Sync WAL before exit
    if let Err(e) = engine.wal().sync() {
        tracing::error!("Failed to sync WAL on shutdown: {}", e);
    }

    tracing::info!("Server stopped");
    Ok(())
}
