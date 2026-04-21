//! Primary-side replication manager.
//!
//! Manages connected replicas, WAL entry streaming, and sync barriers.
//! Uses a WAL-tailing approach: a background task continuously reads new
//! WAL entries and fans them out to all connected replicas.

use crate::auth::TokenValidator;
use crate::config::{ReplicationConfig, ReplicationMode};
use crate::metrics::Metrics;
use crate::replication::protocol::ReplicationMessage;
use crate::stream::MaybeTlsStream;
use dashmap::DashMap;
use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::{Decoder, Encoder};
use rstmdb_wal::WalOffset;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{split, AsyncReadExt, AsyncWrite, AsyncWriteExt};
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
///
/// Filtering is **offset-based**, not sequence-based, because `wal.append()`
/// assigns sequences atomically via `fetch_add` BEFORE the actual disk write
/// completes. Under concurrent writes, sequence N+1 can be written to disk
/// before sequence N, so sequences are not monotonic in disk order. Offsets
/// within a segment, however, are always monotonic (each append appends to
/// the segment's tail).
struct TailPosition {
    /// Highest sequence we've observed. Kept for logging/metrics only.
    sequence: u64,
    /// Exclusive upper bound of tailed offsets: entries with `offset >= this`
    /// are new and should be fanned out; entries with `offset < this` have
    /// already been tailed. Starts at 0 so the very first WAL entry is always
    /// picked up (real entry offsets live in segment ≥1, always > 0).
    next_offset: u64,
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
    /// Wall-clock timestamp (ms) of the most recently tailed WAL entry.
    /// Used by replicas to compute time-based lag.
    latest_write_ts_ms: AtomicU64,
    /// Token validator for replication auth (None = no auth required).
    token_validator: Option<TokenValidator>,
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
        // Initialize to 0 so the very first WAL entry (at segment 1, offset 0
        // = packed 1099511627776) is tailed. Do NOT use `latest_offset()` —
        // that returns the next-write position (segment size), which equals
        // the first entry's offset on an empty WAL and would cause the
        // filter to skip it forever.
        let next_offset = 0u64;

        // Pre-build the token validator from resolved hashes (plaintext token
        // is hashed here too). None means no replication auth is required.
        let token_validator = if config.auth_required() {
            Some(TokenValidator::new(config.resolved_token_hashes()))
        } else {
            None
        };

        let manager = Arc::new(Self {
            config,
            engine,
            replicas: DashMap::new(),
            sync_barriers: Mutex::new(BTreeMap::new()),
            next_replica_id: AtomicU64::new(1),
            tail_position: Mutex::new(TailPosition {
                sequence: last_seq,
                next_offset,
            }),
            latest_write_ts_ms: AtomicU64::new(0),
            token_validator,
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

            let mut pos = self.tail_position.lock().await;

            // Read entries from the segment containing our next-to-tail offset.
            // The filter below skips any entries strictly below `next_offset`.
            let from_offset = WalOffset::from_u64(pos.next_offset);
            let entries = match self.engine.wal().read_from(from_offset, None) {
                Ok(e) => e,
                Err(e) => {
                    tracing::error!("WAL tailer read error: {}", e);
                    continue;
                }
            };

            for (seq, offset, entry) in entries {
                // Offsets within a segment are monotonic on disk (each append
                // appends to the segment tail under segment-mutex). Sequences
                // are NOT monotonic in disk order under concurrent writes.
                if offset.as_u64() < pos.next_offset {
                    continue;
                }

                let now_ms = now_unix_ms();
                self.latest_write_ts_ms.store(now_ms, Ordering::Release);

                let msg = ReplicationMessage::ReplicateEntry {
                    sequence: seq,
                    offset: offset.as_u64(),
                    entry,
                    timestamp_ms: now_ms,
                };

                // Fan out to all connected replicas. If a replica's channel is
                // full, it can't keep up — disconnect it so it reconnects and
                // catches up from WAL instead of silently losing entries.
                let replica_count = self.replicas.len();
                let mut slow_replicas: Vec<String> = Vec::new();
                for replica in self.replicas.iter() {
                    use tokio::sync::mpsc::error::TrySendError;
                    match replica.value().entry_tx.try_send(msg.clone()) {
                        Ok(()) => {}
                        Err(TrySendError::Full(_)) => {
                            slow_replicas.push(replica.key().clone());
                        }
                        Err(TrySendError::Closed(_)) => {
                            // Already torn down; reader/writer tasks will clean up.
                            // Don't warn — this is normal during disconnect.
                        }
                    }
                }

                for replica_id in &slow_replicas {
                    tracing::warn!(
                        "Replica {} cannot keep up (send channel full at seq={}); \
                         disconnecting — will catch up from WAL on reconnect",
                        replica_id,
                        seq,
                    );
                    // Dropping the ReplicaInfo drops its entry_tx sender, which
                    // causes the writer task's entry_rx.recv() to return None,
                    // triggering connection teardown.
                    self.replicas.remove(replica_id);
                    if let Some(ref m) = self.metrics {
                        m.replication_slow_replica_disconnects_total.inc();
                    }
                }

                // Track highest sequence (logging/metrics) and advance
                // next_offset past this entry.
                if seq > pos.sequence {
                    pos.sequence = seq;
                }
                let next = offset.as_u64().saturating_add(1);
                if next > pos.next_offset {
                    pos.next_offset = next;
                }

                if let Some(ref m) = self.metrics {
                    m.replication_entries_sent_total.inc();
                    m.replication_connected_replicas
                        .set(self.replicas.len() as f64);
                }

                tracing::trace!(
                    "Replicated WAL entry seq={} to {} replica(s) ({} dropped as slow)",
                    seq,
                    replica_count - slow_replicas.len(),
                    slow_replicas.len(),
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
    /// Accepts `MaybeTlsStream` so replication works over both plain TCP and TLS.
    pub async fn handle_replica_connection(
        self: &Arc<Self>,
        mut stream: MaybeTlsStream,
        auth_msg: ReplicationMessage,
    ) {
        let (auth_token, last_sequence, last_primary_offset) = match auth_msg {
            ReplicationMessage::ReplicateAuth {
                auth_token,
                last_sequence,
                last_primary_offset,
            } => (auth_token, last_sequence, last_primary_offset),
            _ => {
                tracing::warn!("Expected ReplicateAuth, got unexpected message");
                return;
            }
        };

        // Validate auth token against configured hashes (SHA-256).
        // If no validator is configured, replication auth is disabled.
        if let Some(ref validator) = self.token_validator {
            let presented = auth_token.as_deref().unwrap_or("");
            if !validator.validate(presented) {
                tracing::warn!("Replica authentication failed");
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

        // Split the stream BEFORE catchup. The replica ACKs every entry during
        // catchup; if we don't drain those ACKs concurrently, the primary's TCP
        // recv buffer fills (~64KB, ~few thousand ACKs), TCP flow control kicks
        // in, and the whole catchup stalls mid-way. Spawn the reader up front.
        let (mut read_half, mut write_half) = split(stream);

        // Reader task: reads ACKs from replica (must run concurrently with
        // catchup to avoid the deadlock described above).
        let mgr_reader = self.clone();
        let rid_reader = replica_id.clone();
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
                                mgr_reader.on_ack(&rid_reader, sequence).await;
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Catch-up: send WAL entries the replica doesn't have yet via the write
        // half. Prefer offset-based filtering (monotonic on disk) over sequence
        // (non-monotonic under concurrent writes). Old replicas send
        // last_primary_offset=0 and we fall back to sequence filtering.
        let catch_up_result = self
            .send_catchup(&mut write_half, last_sequence, last_primary_offset)
            .await;

        match &catch_up_result {
            Ok(()) => {
                tracing::info!("Replica {} catch-up complete", replica_id);
            }
            Err(e) => {
                tracing::warn!("Replica {} catch-up failed: {}", replica_id, e);
                self.replicas.remove(&replica_id);
                let _ = write_half.shutdown().await;
                let _ = reader_handle.await;
                return;
            }
        }

        // Live streaming loop
        let mgr = self.clone();

        // Spawn writer task: sends entries from channel to replica
        let writer_handle = tokio::spawn(async move {
            // Heartbeat interval
            let mut heartbeat_interval = tokio::time::interval(mgr.config.heartbeat_interval());

            loop {
                tokio::select! {
                    maybe = entry_rx.recv() => match maybe {
                        Some(msg) => {
                            let bytes = match serde_json::to_vec(&msg) {
                                Ok(b) => b,
                                Err(_) => continue,
                            };
                            let frame = Encoder::encode_raw(&bytes);
                            if write_half.write_all(&frame).await.is_err() {
                                break;
                            }
                        }
                        None => {
                            // Channel closed — primary dropped this replica
                            // (e.g. slow-replica disconnect). Exit so the TCP
                            // socket is shut down cleanly below and the replica
                            // can detect EOF and reconnect.
                            break;
                        }
                    },
                    _ = heartbeat_interval.tick() => {
                        let primary_seq = mgr.engine.wal().next_sequence().saturating_sub(1);
                        let hb = ReplicationMessage::ReplicateHeartbeat {
                            primary_sequence: primary_seq,
                            timestamp_ms: now_unix_ms(),
                            primary_latest_write_ts_ms: mgr.latest_write_ts_ms.load(Ordering::Acquire),
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
            // Ensure the TCP stream is closed so the replica detects disconnect
            // and enters its reconnect loop instead of blocking on read().
            let _ = write_half.shutdown().await;
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
    async fn send_catchup<W>(
        &self,
        stream: &mut W,
        last_sequence: u64,
        last_primary_offset: u64,
    ) -> Result<(), std::io::Error>
    where
        W: AsyncWrite + Unpin,
    {
        // If the replica provided a primary offset (new protocol), read from
        // that offset onwards — avoids scanning the entire WAL. Fall back to
        // reading from 0 for old replicas.
        let read_from = if last_primary_offset > 0 {
            WalOffset::from_u64(last_primary_offset)
        } else {
            WalOffset::from_u64(0)
        };
        let entries = self
            .engine
            .wal()
            .read_from(read_from, None)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let mut sent = 0;
        for (seq, offset, entry) in entries {
            // Prefer offset filter (monotonic on disk) when the replica sent
            // one. Sequence filter is legacy-only and doesn't handle
            // concurrent-write out-of-order sequences.
            if last_primary_offset > 0 {
                if offset.as_u64() <= last_primary_offset {
                    continue;
                }
            } else if seq <= last_sequence {
                continue;
            }

            // Catch-up entries have no original write timestamp available, so
            // we mark them as "now" — meaning the replica will compute lag as
            // ~0 once it applies them. This is fine because catch-up is a
            // burst, not ongoing lag.
            let msg = ReplicationMessage::ReplicateEntry {
                sequence: seq,
                offset: offset.as_u64(),
                entry,
                timestamp_ms: now_unix_ms(),
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

    /// Sends a single replication message over any async stream using RCPX framing.
    async fn send_message<W>(stream: &mut W, msg: &ReplicationMessage) -> Result<(), std::io::Error>
    where
        W: AsyncWrite + Unpin,
    {
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

    /// Returns per-replica stats: `(replica_id, last_acked_sequence, lag_entries)`.
    /// Lag is computed against the current WAL head sequence on the primary.
    pub fn replica_stats(&self) -> Vec<(String, u64, u64)> {
        let primary_seq = self.engine.wal().next_sequence().saturating_sub(1);
        self.replicas
            .iter()
            .map(|r| {
                let acked = r.value().last_acked_sequence.load(Ordering::Acquire);
                let lag = primary_seq.saturating_sub(acked);
                (r.key().clone(), acked, lag)
            })
            .collect()
    }

    /// Returns the timestamp (Unix ms) of the most recently tailed WAL entry.
    pub fn latest_write_ts_ms(&self) -> u64 {
        self.latest_write_ts_ms.load(Ordering::Acquire)
    }
}

/// Returns the current Unix timestamp in milliseconds (0 if clock is before epoch).
fn now_unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
