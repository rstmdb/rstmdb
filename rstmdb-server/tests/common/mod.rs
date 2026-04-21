//! Shared harness for end-to-end replication tests.
//!
//! Spins up a real primary TCP server + replica clients in-process, each with
//! their own WAL under a temp dir. Tests drive writes through the primary's
//! engine (same WAL path the server would use) and wait for replicas to
//! converge before asserting parity.

#![allow(dead_code)] // helpers used by multiple test files

use rstmdb_core::StateMachineEngine;
use rstmdb_server::config::{ReplicationConfig, ReplicationMode, ReplicationRole};
use rstmdb_server::{
    Metrics, ReplicaClient, ReplicationManager, Server, ServerConfig,
};
use rstmdb_wal::{FsyncPolicy, WalConfig};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// A running primary node.
pub struct PrimaryNode {
    pub engine: Arc<StateMachineEngine>,
    pub addr: SocketAddr,
    pub metrics: Arc<Metrics>,
    pub manager: Arc<ReplicationManager>,
    pub shutdown_tx: broadcast::Sender<()>,
    pub _server_handle: JoinHandle<()>,
    pub _wal_dir: TempDir,
}

/// A running replica node.
pub struct ReplicaNode {
    pub engine: Arc<StateMachineEngine>,
    pub metrics: Arc<Metrics>,
    pub shutdown_tx: broadcast::Sender<()>,
    pub client: Arc<ReplicaClient>,
    pub _client_handle: JoinHandle<()>,
    /// Owned tempdir. Kept in an Option so tests can "steal" it to reuse
    /// across simulated restarts (spawn a new replica pointing at the same
    /// WAL files).
    pub wal_dir: Option<TempDir>,
}

/// Options for spawning a primary.
#[derive(Clone, Debug)]
pub struct PrimaryOpts {
    pub poll_interval_ms: u64,
    pub heartbeat_interval_secs: u64,
    pub lag_check_interval_secs: u64,
    pub mode: ReplicationMode,
    pub sync_replicas: u32,
    pub sync_timeout_ms: u64,
    /// Plaintext auth token (if Some, hashed and added to `auth_token_hashes`).
    pub auth_token: Option<String>,
    /// Pre-hashed tokens (SHA-256 hex) accepted by the primary.
    pub auth_token_hashes: Vec<String>,
}

impl Default for PrimaryOpts {
    fn default() -> Self {
        Self {
            poll_interval_ms: 5,
            heartbeat_interval_secs: 1,
            lag_check_interval_secs: 1,
            mode: ReplicationMode::Async,
            sync_replicas: 1,
            sync_timeout_ms: 500,
            auth_token: None,
            auth_token_hashes: Vec::new(),
        }
    }
}

/// Options for spawning a replica.
#[derive(Clone, Debug, Default)]
pub struct ReplicaOpts {
    /// If Some, reuse this wal_dir instead of creating a fresh one.
    pub wal_dir: Option<PathBuf>,
    /// Auth token to send in ReplicateAuth.
    pub auth_token: Option<String>,
}

/// A primary + N replicas, all in-process.
pub struct Cluster {
    pub primary: PrimaryNode,
    pub replicas: Vec<ReplicaNode>,
}

impl Cluster {
    /// Spins up a cluster with `num_replicas` replicas.
    ///
    /// Fast defaults: poll interval 5ms, heartbeat 1s, reconnect 100ms–1s.
    /// No auth. Plain TCP. Each node has its own temp WAL dir.
    pub async fn spawn(num_replicas: usize) -> Self {
        // Pick a free port by binding 0 then releasing
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let primary = Self::spawn_primary_on(addr).await;
        let mut replicas = Vec::with_capacity(num_replicas);
        for _ in 0..num_replicas {
            replicas.push(Self::spawn_replica_for(addr).await);
        }

        let cluster = Self { primary, replicas };
        // Wait for all replicas to connect to the primary
        cluster
            .wait_for_replica_count(num_replicas, Duration::from_secs(5))
            .await;
        cluster
    }

    async fn spawn_primary_on(addr: SocketAddr) -> PrimaryNode {
        Self::spawn_primary_with(addr, PrimaryOpts::default()).await
    }

    /// Spawns a primary with custom options. Useful for tests that exercise
    /// sync mode, auth, or tight intervals.
    pub async fn spawn_primary_with(addr: SocketAddr, opts: PrimaryOpts) -> PrimaryNode {
        let wal_dir = TempDir::new().unwrap();
        let wal_config = WalConfig::new(wal_dir.path())
            .with_segment_size(4 * 1024 * 1024)
            .with_fsync_policy(FsyncPolicy::Never);
        let engine = Arc::new(StateMachineEngine::new(wal_config).unwrap());
        let metrics = Arc::new(Metrics::new().unwrap());

        let repl_config = ReplicationConfig {
            role: ReplicationRole::Primary,
            mode: opts.mode,
            sync_replicas: opts.sync_replicas,
            sync_timeout_ms: opts.sync_timeout_ms,
            poll_interval_ms: opts.poll_interval_ms,
            heartbeat_interval_secs: opts.heartbeat_interval_secs,
            lag_check_interval_secs: opts.lag_check_interval_secs,
            auth_token: opts.auth_token.clone(),
            auth_token_hashes: opts.auth_token_hashes.clone(),
            ..Default::default()
        };

        let (shutdown_tx, shutdown_rx) = broadcast::channel(4);
        let mgr_shutdown_rx = shutdown_tx.subscribe();
        let manager = ReplicationManager::new(
            repl_config,
            engine.clone(),
            mgr_shutdown_rx,
            Some(metrics.clone()),
        );

        let mut server_config = ServerConfig::new(addr);
        server_config.allow_flush_all = true;
        server_config.metrics = Some(metrics.clone());
        let mut server = Server::new(server_config, engine.clone());
        server.set_replication_manager(manager.clone());
        let server = Arc::new(server);

        // Spawn server in background
        let server_clone = server.clone();
        let handle = tokio::spawn(async move {
            let _ = server_clone.run().await;
        });

        // Wait briefly for the server to be listening
        wait_until_listening(addr, Duration::from_secs(2)).await;

        // Ensure shutdown flows to server too
        let server_shutdown = server.clone();
        tokio::spawn(async move {
            let _ = shutdown_rx.resubscribe().recv().await;
            server_shutdown.shutdown();
        });

        PrimaryNode {
            engine,
            addr,
            metrics,
            manager,
            shutdown_tx,
            _server_handle: handle,
            _wal_dir: wal_dir,
        }
    }

    async fn spawn_replica_for(primary_addr: SocketAddr) -> ReplicaNode {
        Self::spawn_replica_with(primary_addr, ReplicaOpts::default()).await
    }

    /// Spawns a replica with custom options. Use `ReplicaOpts.wal_dir` to
    /// reuse a persistent WAL directory (simulates restart-with-state).
    pub async fn spawn_replica_with(primary_addr: SocketAddr, opts: ReplicaOpts) -> ReplicaNode {
        let (wal_path, wal_dir_owned): (PathBuf, Option<TempDir>) = match opts.wal_dir {
            Some(p) => (p, None),
            None => {
                let td = TempDir::new().unwrap();
                (td.path().to_path_buf(), Some(td))
            }
        };
        let wal_config = WalConfig::new(&wal_path)
            .with_segment_size(4 * 1024 * 1024)
            .with_fsync_policy(FsyncPolicy::Never);
        let engine = Arc::new(StateMachineEngine::new(wal_config).unwrap());
        let metrics = Arc::new(Metrics::new().unwrap());

        let repl_config = ReplicationConfig {
            role: ReplicationRole::Replica,
            upstream: Some(primary_addr.to_string()),
            reconnect_delay_secs: 0,
            reconnect_max_delay_secs: 1,
            lag_check_interval_secs: 1,
            auth_token: opts.auth_token.clone(),
            ..Default::default()
        };

        let client = ReplicaClient::new(
            repl_config,
            engine.clone(),
            primary_addr.to_string(),
            opts.auth_token,
        )
        .unwrap();

        let (shutdown_tx, shutdown_rx) = broadcast::channel(4);
        let client_arc = Arc::new(client);
        let client_clone = client_arc.clone();
        let handle = tokio::spawn(async move {
            client_clone.run(shutdown_rx).await;
        });

        ReplicaNode {
            engine,
            metrics,
            shutdown_tx,
            client: client_arc,
            _client_handle: handle,
            wal_dir: wal_dir_owned,
        }
    }

    /// Waits until the primary reports `expected` connected replicas.
    pub async fn wait_for_replica_count(&self, expected: usize, timeout: Duration) {
        let start = Instant::now();
        while start.elapsed() < timeout {
            if self.primary.manager.connected_replica_count() == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "timeout waiting for {} replicas, only {} connected",
            expected,
            self.primary.manager.connected_replica_count()
        );
    }

    /// Polls until every replica's primary-offset matches the primary's latest
    /// WAL offset (i.e. fully caught up). Returns on success, panics on timeout.
    pub async fn wait_converged(&self, timeout: Duration) {
        let start = Instant::now();
        loop {
            let primary_wal_entries =
                self.primary.engine.wal().next_sequence().saturating_sub(1);

            let all_caught_up = self.replicas.iter().all(|r| {
                let replica_wal_entries = r.engine.wal().next_sequence().saturating_sub(1);
                replica_wal_entries == primary_wal_entries
            });

            if all_caught_up {
                return;
            }

            if start.elapsed() > timeout {
                let primary_count = primary_wal_entries;
                let replica_counts: Vec<u64> = self
                    .replicas
                    .iter()
                    .map(|r| r.engine.wal().next_sequence().saturating_sub(1))
                    .collect();
                panic!(
                    "cluster did not converge in {:?} — primary has {} WAL entries, \
                     replicas have {:?}",
                    timeout, primary_count, replica_counts
                );
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Asserts that every replica mirrors the primary's in-memory state:
    /// same machines, same instance IDs with same state + context.
    pub fn assert_parity(&self) {
        let primary_machines = self.primary.engine.list_machines();
        let primary_instances = self.primary.engine.get_all_instances();

        for (idx, replica) in self.replicas.iter().enumerate() {
            let rm = replica.engine.list_machines();
            assert_eq!(
                primary_machines, rm,
                "replica #{} has different machines than primary: {:?} vs {:?}",
                idx, rm, primary_machines
            );

            let ri = replica.engine.get_all_instances();
            assert_eq!(
                primary_instances.len(),
                ri.len(),
                "replica #{} has {} instances, primary has {}",
                idx,
                ri.len(),
                primary_instances.len(),
            );

            // Build maps by id for order-independent comparison
            let primary_by_id: std::collections::HashMap<_, _> =
                primary_instances.iter().map(|i| (&i.id, i)).collect();
            for replica_inst in &ri {
                let pi = primary_by_id.get(&replica_inst.id).unwrap_or_else(|| {
                    panic!(
                        "replica #{} has instance {} that primary doesn't have",
                        idx, replica_inst.id
                    )
                });
                assert_eq!(
                    pi.state, replica_inst.state,
                    "replica #{} instance {} state mismatch",
                    idx, replica_inst.id
                );
                assert_eq!(
                    pi.ctx, replica_inst.ctx,
                    "replica #{} instance {} ctx mismatch",
                    idx, replica_inst.id
                );
                assert_eq!(
                    pi.machine, replica_inst.machine,
                    "replica #{} instance {} machine mismatch",
                    idx, replica_inst.id
                );
                assert_eq!(
                    pi.version, replica_inst.version,
                    "replica #{} instance {} version mismatch",
                    idx, replica_inst.id
                );
                // wal_offset should match too — the primary_offset fix
                assert_eq!(
                    pi.last_wal_offset, replica_inst.last_wal_offset,
                    "replica #{} instance {} wal_offset mismatch",
                    idx, replica_inst.id
                );
            }
        }
    }

    /// Shuts down all nodes.
    pub fn shutdown(self) {
        let _ = self.primary.shutdown_tx.send(());
        for r in &self.replicas {
            let _ = r.shutdown_tx.send(());
        }
    }
}

async fn wait_until_listening(addr: SocketAddr, timeout: Duration) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if tokio::net::TcpStream::connect(addr).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("primary not listening on {:?} after {:?}", addr, timeout);
}

pub fn order_machine_def() -> Value {
    json!({
        "states": ["pending", "paid", "shipped", "delivered"],
        "initial": "pending",
        "transitions": [
            {"from": "pending", "event": "PAY", "to": "paid"},
            {"from": "paid", "event": "SHIP", "to": "shipped"},
            {"from": "shipped", "event": "DELIVER", "to": "delivered"}
        ]
    })
}
