//! Primary-side replication manager.
//!
//! Manages connected replicas, WAL entry streaming, and sync barriers.
//! Uses a WAL-tailing approach: a background task continuously reads new
//! WAL entries and fans them out to all connected replicas.

use crate::config::{ReplicationConfig, ReplicationMode};
use crate::metrics::Metrics;
use crate::replication::protocol::ReplicationMessage;
use dashmap::DashMap;
use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::{Decoder, Encoder};
use rstmdb_wal::WalOffset;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, Mutex};

/// Information about a connected replica.
struct ReplicaInfo {
    /// Channel to send entries to this replica's streaming task.
    entry_tx: mpsc::Sender<ReplicationMessage>,
    /// Last sequence ACKed by this replica.
    last_acked_sequence: Arc<AtomicU64>,
}

/// A pending sync barrier waiting for replica ACKs.
struct SyncBarrier {
    senders: Vec<oneshot::Sender<()>>,
}

/// Tracks the WAL position the tailer has streamed up to.
struct TailPosition {
    /// The sequence of the last entry we tailed. 0 if nothing tailed yet.
    sequence: u64,
    /// The offset of the last entry we tailed. Used as a lower bound for the
    /// next `read_from` call; the sequence filter skips the already-tailed entry.
    last_offset: u64,
}

/// Primary-side replication manager.
pub struct ReplicationManager {
    config: ReplicationConfig,
    engine: Arc<StateMachineEngine>,
    /// Connected replicas by ID.
    replicas: DashMap<String, ReplicaInfo>,
    /// Sync barriers: sequence -> barrier (used in sync mode).
    sync_barriers: Mutex<BTreeMap<u64, SyncBarrier>>,
    /// Next replica ID counter.
    next_replica_id: AtomicU64,
    /// WAL tailer position — protected by mutex since only the tailer task writes it.
    tail_position: Mutex<TailPosition>,
    /// Optional metrics for replication counters.
    metrics: Option<Arc<Metrics>>,
}

impl ReplicationManager {
    /// Creates a new replication manager and spawns the WAL tailer.
    /// The tailer stops when the shutdown signal is received.
    pub fn new(
        config: ReplicationConfig,
        engine: Arc<StateMachineEngine>,
        shutdown: tokio::sync::broadcast::Receiver<()>,
        metrics: Option<Arc<Metrics>>,
    ) -> Arc<Self> {
        let last_seq = engine.wal().next_sequence().saturating_sub(1);
        let last_offset = engine.wal().latest_offset().map(|o| o.as_u64()).unwrap_or(0);

        let manager = Arc::new(Self {
            config,
            engine,
            replicas: DashMap::new(),
            sync_barriers: Mutex::new(BTreeMap::new()),
            next_replica_id: AtomicU64::new(1),
            tail_position: Mutex::new(TailPosition {
                sequence: last_seq,
                last_offset,
            }),
            metrics,
        });

        // Spawn the WAL tailer that watches for new entries and fans out
        let mgr = manager.clone();
        tokio::spawn(async move {
            mgr.wal_tailer_task(shutdown).await;
        });

        manager
    }

    /// Background task that polls the WAL for new entries and sends them to all replicas.
    async fn wal_tailer_task(&self, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        let mut interval = tokio::time::interval(self.config.poll_interval());

        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = shutdown.recv() => {
                    tracing::info!("WAL tailer shutting down");
                    return;
                }
            }

            if self.replicas.is_empty() {
                continue;
            }

            let current_seq = self.engine.wal().next_sequence().saturating_sub(1);

            let mut pos = self.tail_position.lock().await;
            if current_seq <= pos.sequence {
                continue;
            }

            // Read entries from the segment containing our last-tailed offset.
            // The sequence filter below skips the already-tailed entry itself.
            let from_offset = WalOffset::from_u64(pos.last_offset);
            let entries = match self.engine.wal().read_from(from_offset, None) {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!("WAL tailer read error: {}", e);
                    continue;
                }
            };

            for (seq, offset, entry) in entries {
                if seq <= pos.sequence {
                    continue;
                }

                let msg = ReplicationMessage::ReplicateEntry {
                    sequence: seq,
                    offset: offset.as_u64(),
                    entry,
                };

                // Fan out to all connected replicas
                let replica_count = self.replicas.len();
                for replica in self.replicas.iter() {
                    let _ = replica.value().entry_tx.try_send(msg.clone());
                }

                pos.sequence = seq;
                pos.last_offset = offset.as_u64();

                if let Some(ref m) = self.metrics {
                    m.replication_entries_sent_total.inc();
                    m.replication_connected_replicas
                        .set(replica_count as f64);
                }

                tracing::trace!(
                    "Replicated WAL entry seq={} to {} replica(s)",
                    seq,
                    replica_count
                );
            }
        }
    }

    /// Called when a replica ACKs a sequence number.
    async fn on_ack(&self, replica_id: &str, sequence: u64) {
        if let Some(replica) = self.replicas.get(replica_id) {
            replica
                .last_acked_sequence
                .fetch_max(sequence, Ordering::Release);
        }
        self.check_and_resolve_barriers(sequence).await;
    }

    /// Checks if enough replicas have ACKed to resolve barriers at or below the given sequence.
    async fn check_and_resolve_barriers(&self, up_to_sequence: u64) {
        let required_acks = self.config.sync_replicas as usize;

        let mut barriers = self.sync_barriers.lock().await;
        let mut resolved_sequences = Vec::new();

        for (&seq, _) in barriers.iter() {
            if seq > up_to_sequence {
                break;
            }

            // Count how many replicas have ACKed >= this sequence
            let ack_count = self
                .replicas
                .iter()
                .filter(|r| r.last_acked_sequence.load(Ordering::Acquire) >= seq)
                .count();

            if ack_count >= required_acks {
                resolved_sequences.push(seq);
            }
        }

        // Resolve all satisfied barriers
        for seq in resolved_sequences {
            if let Some(barrier) = barriers.remove(&seq) {
                for sender in barrier.senders {
                    let _ = sender.send(());
                }
            }
        }
    }

    /// Handles a new replication connection from a replica.
    pub async fn handle_replica_connection(
        self: &Arc<Self>,
        mut stream: TcpStream,
        auth_msg: ReplicationMessage,
    ) {
        let (auth_token, last_sequence) = match auth_msg {
            ReplicationMessage::ReplicateAuth {
                auth_token,
                last_sequence,
            } => (auth_token, last_sequence),
            _ => {
                tracing::warn!("Expected ReplicateAuth, got unexpected message");
                return;
            }
        };

        // Validate auth token
        if let Some(ref expected) = self.config.auth_token {
            if auth_token.as_deref() != Some(expected.as_str()) {
                let resp = ReplicationMessage::ReplicateSyncResponse {
                    ok: false,
                    primary_sequence: 0,
                    error: Some("authentication failed".to_string()),
                };
                let _ = Self::send_message(&mut stream, &resp).await;
                return;
            }
        }

        let primary_sequence = self.engine.wal().next_sequence().saturating_sub(1);
        let replica_id = format!(
            "replica-{}",
            self.next_replica_id.fetch_add(1, Ordering::Relaxed)
        );

        // Send sync response
        let resp = ReplicationMessage::ReplicateSyncResponse {
            ok: true,
            primary_sequence,
            error: None,
        };
        if Self::send_message(&mut stream, &resp).await.is_err() {
            return;
        }

        tracing::info!(
            "Replica {} connected, last_sequence={}, primary_sequence={}",
            replica_id,
            last_sequence,
            primary_sequence
        );

        // Create entry channel for this replica
        let (entry_tx, mut entry_rx) =
            mpsc::channel::<ReplicationMessage>(super::REPLICA_CHANNEL_CAPACITY);
        let last_acked = Arc::new(AtomicU64::new(0));

        self.replicas.insert(
            replica_id.clone(),
            ReplicaInfo {
                entry_tx,
                last_acked_sequence: last_acked.clone(),
            },
        );

        // Catch-up: send WAL entries from replica's last_sequence
        let catch_up_result = self.send_catchup(&mut stream, last_sequence).await;

        match &catch_up_result {
            Ok(()) => {
                tracing::info!("Replica {} catch-up complete", replica_id);
            }
            Err(e) => {
                tracing::warn!("Replica {} catch-up failed: {}", replica_id, e);
                self.replicas.remove(&replica_id);
                return;
            }
        }

        // Live streaming loop
        let (mut read_half, mut write_half) = stream.into_split();
        let mgr = self.clone();

        // Spawn writer task: sends entries from channel to replica
        let writer_handle = tokio::spawn(async move {
            // Heartbeat interval
            let mut heartbeat_interval = tokio::time::interval(mgr.config.heartbeat_interval());

            loop {
                tokio::select! {
                    Some(msg) = entry_rx.recv() => {
                        let bytes = match serde_json::to_vec(&msg) {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        let frame = Encoder::encode_raw(&bytes);
                        if write_half.write_all(&frame).await.is_err() {
                            break;
                        }
                    }
                    _ = heartbeat_interval.tick() => {
                        let primary_seq = mgr.engine.wal().next_sequence().saturating_sub(1);
                        let hb = ReplicationMessage::ReplicateHeartbeat {
                            primary_sequence: primary_seq,
                            timestamp_ms: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .unwrap_or_default()
                                .as_millis() as u64,
                        };
                        let bytes = match serde_json::to_vec(&hb) {
                            Ok(b) => b,
                            Err(_) => continue,
                        };
                        let frame = Encoder::encode_raw(&bytes);
                        if write_half.write_all(&frame).await.is_err() {
                            break;
                        }
                    }
                    else => break,
                }
            }
        });

        // Reader task: reads ACKs from replica
        let mgr2 = self.clone();
        let rid2 = replica_id.clone();
        let reader_handle = tokio::spawn(async move {
            let mut decoder = Decoder::new();
            let mut buf = [0u8; super::REPLICATION_READ_BUF_SIZE];

            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        decoder.extend(&buf[..n]);
                        while let Ok(Some(payload)) = decoder.decode_raw() {
                            if let Ok(ReplicationMessage::ReplicateAck { sequence }) =
                                ReplicationMessage::from_bytes(&payload)
                            {
                                mgr2.on_ack(&rid2, sequence).await;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Wait for either task to finish
        tokio::select! {
            _ = writer_handle => {},
            _ = reader_handle => {},
        }

        tracing::info!("Replica {} disconnected", replica_id);
        self.replicas.remove(&replica_id);
    }

    /// Sends catch-up entries from the WAL to a newly connected replica.
    async fn send_catchup(
        &self,
        stream: &mut TcpStream,
        last_sequence: u64,
    ) -> Result<(), std::io::Error> {
        let entries = self
            .engine
            .wal()
            .read_from(WalOffset::from_u64(0), None)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let mut sent = 0;
        for (seq, offset, entry) in entries {
            if seq <= last_sequence {
                continue;
            }

            let msg = ReplicationMessage::ReplicateEntry {
                sequence: seq,
                offset: offset.as_u64(),
                entry,
            };
            Self::send_message(stream, &msg).await?;
            sent += 1;
        }

        if sent > 0 {
            stream.flush().await?;
            tracing::info!("Sent {} catch-up entries to replica", sent);
        }

        Ok(())
    }

    /// Sends a single replication message over a stream using RCPX framing.
    async fn send_message(
        stream: &mut TcpStream,
        msg: &ReplicationMessage,
    ) -> Result<(), std::io::Error> {
        let bytes = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
        let frame = Encoder::encode_raw(&bytes);
        stream.write_all(&frame).await
    }

    /// Waits until the current WAL head sequence has been ACKed by enough replicas,
    /// or returns an error on timeout. Used by the server for sync replication mode.
    pub async fn await_replication(&self) -> Result<(), String> {
        let sequence = self.engine.wal().next_sequence().saturating_sub(1);
        if sequence == 0 {
            return Ok(());
        }

        if self.replicas.is_empty() {
            return Err("no replicas connected for sync replication".to_string());
        }

        // Check if already satisfied
        let ack_count = self
            .replicas
            .iter()
            .filter(|r| r.last_acked_sequence.load(Ordering::Acquire) >= sequence)
            .count();
        if ack_count >= self.config.sync_replicas as usize {
            return Ok(());
        }

        // Create barrier
        let (tx, rx) = oneshot::channel();
        {
            let mut barriers = self.sync_barriers.lock().await;
            let barrier = barriers.entry(sequence).or_insert_with(|| SyncBarrier {
                senders: Vec::new(),
            });
            barrier.senders.push(tx);
        }

        // Re-check after registering (race with ACKs arriving)
        self.check_and_resolve_barriers(sequence).await;

        // Wait with timeout
        let timeout = self.config.sync_timeout();
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => Err("sync barrier dropped".to_string()),
            Err(_) => {
                // Timeout — clean up barrier
                let mut barriers = self.sync_barriers.lock().await;
                barriers.remove(&sequence);

                if let Some(ref m) = self.metrics {
                    m.replication_sync_timeouts_total.inc();
                }

                Err(format!(
                    "sync replication timeout after {}ms (sequence={}, need {} ACKs)",
                    self.config.sync_timeout_ms, sequence, self.config.sync_replicas
                ))
            }
        }
    }

    /// Returns the number of connected replicas.
    pub fn connected_replica_count(&self) -> usize {
        self.replicas.len()
    }

    /// Returns whether we're in sync replication mode.
    pub fn is_sync(&self) -> bool {
        self.config.mode == ReplicationMode::Sync
    }

    /// Returns the replication config.
    pub fn config(&self) -> &ReplicationConfig {
        &self.config
    }
}
