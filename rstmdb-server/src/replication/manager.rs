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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{split, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex};

/// Information about a connected replica.
struct ReplicaInfo {
    /// Channel to send entries to this replica's streaming task.
    entry_tx: mpsc::Sender<ReplicationMessage>,
    /// Last sequence ACKed by this replica. Kept for lag observability only.
    last_acked_sequence: Arc<AtomicU64>,
    /// Highest **primary WAL offset** ACKed by this replica. This — not the
    /// sequence — is what the sync barrier waits on: offsets are monotonic on
    /// disk and applied in order, so "acked offset >= X" durably covers every
    /// entry up to X. Sequences can't provide that guarantee under concurrent
    /// writes (sequence N+1 can land at a lower offset than N).
    last_acked_offset: Arc<AtomicU64>,
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
    /// Next replica ID counter.
    next_replica_id: AtomicU64,
    /// WAL tailer position — protected by mutex since only the tailer task writes it.
    tail_position: Mutex<TailPosition>,
    /// Highest **offset** the tailer has streamed to replicas. The sync barrier
    /// targets this: once a write's entry has been streamed, this is >= that
    /// entry's offset, so waiting for replicas to ack up to it durably covers
    /// the write regardless of sequence/offset non-monotonicity.
    latest_streamed_offset: AtomicU64,
    /// Highest **sequence** the tailer has streamed. Used by the barrier to
    /// confirm a just-made write has actually been picked up by the tailer
    /// before it samples `latest_streamed_offset`.
    latest_streamed_sequence: AtomicU64,
    /// Woken whenever the streamed high-water advances (tailer/catch-up) or a
    /// replica acks a higher offset. `await_replication` waits on this instead
    /// of polling, so sync writes complete the instant durability is reached.
    barrier_notify: tokio::sync::Notify,
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
    ///
    /// `fallback_validator` is the server's **client-auth** token validator (or
    /// `None` if the server requires no client auth). Replication auth resolves
    /// as: replication-specific tokens if configured, else the client-auth
    /// validator, else no auth. This ensures replication is never a side-door
    /// around client auth — if the operator requires client auth but forgets to
    /// set a replication token, replicas must still authenticate (with client
    /// credentials) rather than streaming the WAL to anyone.
    pub fn new(
        config: ReplicationConfig,
        engine: Arc<StateMachineEngine>,
        shutdown: tokio::sync::broadcast::Receiver<()>,
        metrics: Option<Arc<Metrics>>,
        fallback_validator: Option<TokenValidator>,
    ) -> Arc<Self> {
        let last_seq = engine.wal().next_sequence().saturating_sub(1);
        // Initialize to 0 so the very first WAL entry (at segment 1, offset 0
        // = packed 1099511627776) is tailed. Do NOT use `latest_offset()` —
        // that returns the next-write position (segment size), which equals
        // the first entry's offset on an empty WAL and would cause the
        // filter to skip it forever.
        let next_offset = 0u64;

        // Resolve the replication token validator. Replication-specific auth
        // (plaintext token hashed here too) wins; otherwise fall back to the
        // server's client-auth validator so replication can never be less
        // protected than the server's overall auth posture. `None` on both
        // means the server has no auth at all.
        let token_validator = if config.auth_required() {
            Some(TokenValidator::new(config.resolved_token_hashes()))
        } else {
            fallback_validator
        };

        let manager = Arc::new(Self {
            config,
            engine,
            replicas: DashMap::new(),
            next_replica_id: AtomicU64::new(1),
            tail_position: Mutex::new(TailPosition {
                sequence: last_seq,
                next_offset,
            }),
            latest_streamed_offset: AtomicU64::new(0),
            // Starts at 0 (not last_seq): the barrier only trusts offsets that
            // have actually been streamed, so the tailer/catch-up must advance
            // this before a write is considered replicated.
            latest_streamed_sequence: AtomicU64::new(0),
            barrier_notify: tokio::sync::Notify::new(),
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

            // Fan out only when replicas are connected — but ALWAYS advance the
            // tail cursor below, even with no replicas. Otherwise the cursor
            // would freeze at its old position and a replica that joins later
            // (after catching up out-of-band) would trigger a full replay of all
            // history into its bounded channel and overflow it (the H4 hazard).
            let have_replicas = !self.replicas.is_empty();

            for (seq, offset, entry) in entries {
                // Offsets within a segment are monotonic on disk (each append
                // appends to the segment tail under segment-mutex). Sequences
                // are NOT monotonic in disk order under concurrent writes.
                if offset.as_u64() < pos.next_offset {
                    continue;
                }

                if have_replicas {
                    let now_ms = now_unix_ms();
                    self.latest_write_ts_ms.store(now_ms, Ordering::Release);

                    let msg = ReplicationMessage::ReplicateEntry {
                        sequence: seq,
                        offset: offset.as_u64(),
                        entry,
                        timestamp_ms: now_ms,
                    };

                    // Fan out to all connected replicas. If a replica's channel
                    // is full, it can't keep up with LIVE load — disconnect it so
                    // it reconnects and catches up from WAL. (Replicas still
                    // catching up are NOT in this map, so they can't be hit here.)
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
                                // Already torn down; reader/writer tasks clean up.
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
                        self.replicas.remove(replica_id);
                        if let Some(ref m) = self.metrics {
                            m.replication_slow_replica_disconnects_total.inc();
                        }
                    }

                    // Publish streamed high-water marks for the sync barrier.
                    self.latest_streamed_offset
                        .fetch_max(offset.as_u64(), Ordering::AcqRel);
                    self.latest_streamed_sequence
                        .fetch_max(seq, Ordering::AcqRel);
                    self.barrier_notify.notify_waiters();

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

                // Always advance the tail cursor past this entry.
                if seq > pos.sequence {
                    pos.sequence = seq;
                }
                let next = offset.as_u64().saturating_add(1);
                if next > pos.next_offset {
                    pos.next_offset = next;
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

        // Shared ACK watermarks, updated by the reader during BOTH catch-up and
        // live streaming. Created before catch-up (and before the replica joins
        // the fan-out map) so catch-up ACKs aren't lost — when the replica later
        // joins the map its offset watermark already reflects catch-up progress,
        // so a sync barrier can resolve even if no live writes follow.
        //
        // Seed them with the position the replica reported in its handshake: a
        // reconnecting replica that is already caught up sends no ACKs (there's
        // nothing to re-stream), so starting from 0 would leave it looking
        // permanently lagged (`lag = primary_seq - 0`) even though it holds all
        // the data. Seeding from the handshake reflects what the replica already
        // has; subsequent ACKs only advance it.
        let last_acked_sequence = Arc::new(AtomicU64::new(last_sequence));
        let last_acked_offset = Arc::new(AtomicU64::new(last_primary_offset));

        // Split the stream BEFORE catch-up. The replica ACKs every entry during
        // catch-up; if we don't drain those ACKs concurrently, the primary's TCP
        // recv buffer fills (~64KB, ~few thousand ACKs), TCP flow control kicks
        // in, and the whole catch-up stalls mid-way. Spawn the reader up front.
        let (mut read_half, mut write_half) = split(stream);

        // Reader task: updates the ACK watermarks and wakes any sync barrier.
        // It shares the watermark Arcs (not a map lookup) so ACKs are recorded
        // even while the replica has not yet joined the fan-out map.
        let mgr_reader = self.clone();
        let ack_seq = last_acked_sequence.clone();
        let ack_off = last_acked_offset.clone();
        let reader_handle = tokio::spawn(async move {
            let mut decoder = Decoder::new();
            let mut buf = [0u8; super::REPLICATION_READ_BUF_SIZE];

            loop {
                match read_half.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        decoder.extend(&buf[..n]);
                        while let Ok(Some(payload)) = decoder.decode_raw() {
                            if let Ok(ReplicationMessage::ReplicateAck {
                                sequence,
                                applied_offset,
                            }) = ReplicationMessage::from_bytes(&payload)
                            {
                                ack_seq.fetch_max(sequence, Ordering::Release);
                                if applied_offset > 0 {
                                    let prev = ack_off.fetch_max(applied_offset, Ordering::Release);
                                    if applied_offset > prev {
                                        mgr_reader.barrier_notify.notify_waiters();
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        // Catch up to the live tail WITHOUT joining the fan-out map, so live
        // writes during a long catch-up can't overflow a bounded channel and
        // trigger a spurious slow-replica disconnect (the H4 livelock). Streams
        // straight from the WAL by offset cursor, looping until it converges.
        let cursor = match self
            .catch_up_until_converged(&mut write_half, last_sequence, last_primary_offset)
            .await
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("Replica {} catch-up failed: {}", replica_id, e);
                let _ = write_half.shutdown().await;
                let _ = reader_handle.await;
                return;
            }
        };
        tracing::info!("Replica {} catch-up complete", replica_id);

        // Now caught up: join the fan-out. Live entries flow via the channel; if
        // this replica can't keep up with LIVE load, the tailer's bounded channel
        // fills and it's disconnected as genuinely slow (the intended behavior).
        let (entry_tx, mut entry_rx) =
            mpsc::channel::<ReplicationMessage>(super::REPLICA_CHANNEL_CAPACITY);
        self.replicas.insert(
            replica_id.clone(),
            ReplicaInfo {
                entry_tx,
                last_acked_sequence: last_acked_sequence.clone(),
                last_acked_offset: last_acked_offset.clone(),
            },
        );

        // Gap-fill: entries written between convergence and joining the map are
        // not in the channel (the tailer fans out only from its current position
        // onward). Stream anything the WAL has past our cursor; the replica
        // dedups overlaps with the tailer's live sends by offset.
        //
        // Pass the ORIGINAL `last_sequence` (not 0): when the replica sent
        // `last_primary_offset == 0` (e.g. after a restart), the first pass
        // filtered by sequence and left `cursor == 0`, so the gap-fill would run
        // the sequence filter too. With `last_sequence == 0` it would match
        // `seq <= 0` (nothing), re-streaming the ENTIRE WAL every restart —
        // re-applying every entry and bloating the replica's WAL. Using the
        // replica's real `last_sequence` keeps the seq filter correct, and when
        // `cursor > 0` the offset filter is used and `last_sequence` is ignored.
        if let Err(e) = self
            .catch_up_until_converged(&mut write_half, last_sequence, cursor)
            .await
        {
            tracing::warn!("Replica {} gap-fill failed: {}", replica_id, e);
            self.replicas.remove(&replica_id);
            let _ = write_half.shutdown().await;
            let _ = reader_handle.await;
            return;
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

    /// Streams catch-up entries to a replica, repeatedly, until it converges on
    /// the current WAL head — i.e. a pass sends nothing new. Returns the final
    /// offset cursor reached.
    ///
    /// This runs while the replica is NOT in the fan-out map, so live writes
    /// during a long catch-up cannot overflow a bounded channel and trigger a
    /// spurious slow-replica disconnect (the H4 livelock). Each pass reads the
    /// WAL directly, so it is naturally flow-controlled by TCP backpressure.
    async fn catch_up_until_converged<W>(
        &self,
        stream: &mut W,
        last_sequence: u64,
        from_offset: u64,
    ) -> Result<u64, std::io::Error>
    where
        W: AsyncWrite + Unpin,
    {
        let mut cursor = from_offset;
        loop {
            let new_cursor = self
                .send_catchup_pass(stream, last_sequence, cursor)
                .await?;
            // No offset advance ⇒ nothing new was sent ⇒ caught up.
            if new_cursor == cursor {
                break;
            }
            cursor = new_cursor;
        }
        Ok(cursor)
    }

    /// Sends one catch-up pass: every WAL entry after `from_offset` (or, when
    /// `from_offset == 0`, every entry after `last_sequence` — the legacy
    /// sequence filter). Returns the highest offset sent, or `from_offset` if
    /// nothing was sent.
    async fn send_catchup_pass<W>(
        &self,
        stream: &mut W,
        last_sequence: u64,
        from_offset: u64,
    ) -> Result<u64, std::io::Error>
    where
        W: AsyncWrite + Unpin,
    {
        let entries = self
            .engine
            .wal()
            .read_from(WalOffset::from_u64(from_offset), None)
            .map_err(|e| std::io::Error::other(e.to_string()))?;

        let mut max_offset = from_offset;
        let mut sent = 0;
        for (seq, offset, entry) in entries {
            let o = offset.as_u64();
            // Prefer offset filter (monotonic on disk) when we have a cursor.
            // Sequence filter is legacy-only (replica sent no offset) and
            // doesn't handle concurrent-write out-of-order sequences.
            if from_offset > 0 {
                if o <= from_offset {
                    continue;
                }
            } else if seq <= last_sequence {
                continue;
            }

            // Catch-up entries have no original write timestamp available, so
            // we mark them as "now" — the replica computes lag as ~0 once it
            // applies them. Fine, because catch-up is a burst, not ongoing lag.
            let msg = ReplicationMessage::ReplicateEntry {
                sequence: seq,
                offset: o,
                entry,
                timestamp_ms: now_unix_ms(),
            };
            Self::send_message(stream, &msg).await?;
            // Catch-up also advances the streamed high-water: a write can reach
            // replicas via catch-up rather than the live tailer, and the sync
            // barrier must account for it.
            self.latest_streamed_offset.fetch_max(o, Ordering::AcqRel);
            self.latest_streamed_sequence
                .fetch_max(seq, Ordering::AcqRel);
            self.barrier_notify.notify_waiters();
            if o > max_offset {
                max_offset = o;
            }
            sent += 1;
        }

        if sent > 0 {
            stream.flush().await?;
            tracing::info!("Sent {} catch-up entries to replica", sent);
        }

        Ok(max_offset)
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

    /// Waits until the current WAL head is durable on enough replicas, or
    /// returns an error on timeout. Used by the server for sync replication.
    ///
    /// Durability is tracked by **offset**, not sequence. Sequences are assigned
    /// by `fetch_add` before the disk write, so under concurrent writes sequence
    /// N+1 can land at a lower offset than N. A replica acking a sequence at or
    /// above the head therefore does NOT imply it has applied every entry the
    /// primary has — a lower-sequence, higher-offset write can still be missing.
    /// Offsets are monotonic on disk and applied in order, so a replica acking
    /// an offset at or above the target durably covers everything up to it.
    ///
    /// Two phases: (1) wait for the tailer/catch-up to have streamed our write
    /// (by sequence), so the streamed-offset high-water includes it; then
    /// (2) wait for `sync_replicas` replicas to ack an offset at or above that
    /// high-water. Both phases wait on `barrier_notify` (woken by the tailer,
    /// catch-up, and acks) rather than polling.
    ///
    /// Note on compatibility: a legacy replica that acks with `applied_offset =
    /// 0` (pre-upgrade wire format) cannot advance the offset barrier and so
    /// cannot count toward a sync quorum — the write will time out. This is the
    /// safe outcome (we cannot prove durability by offset for such a replica),
    /// but it means sync mode requires offset-aware replicas; during a rolling
    /// upgrade, sync writes may time out until replicas are upgraded.
    pub async fn await_replication(&self) -> Result<(), String> {
        let head_sequence = self.engine.wal().next_sequence().saturating_sub(1);
        if head_sequence == 0 {
            return Ok(());
        }

        if self.replicas.is_empty() {
            return Err("no replicas connected for sync replication".to_string());
        }

        let required_acks = self.config.sync_replicas as usize;
        let timeout = self.config.sync_timeout();

        let waited = tokio::time::timeout(timeout, async {
            // Phase 1: ensure our write has actually been streamed, so the
            // offset high-water reflects it (regardless of seq/offset ordering).
            loop {
                // Register interest BEFORE checking, so a notify that fires
                // between the check and the await is not lost.
                let notified = self.barrier_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                if self.latest_streamed_sequence.load(Ordering::Acquire) >= head_sequence {
                    break;
                }
                notified.await;
            }
            // Every entry up to our write now has offset <= this high-water.
            let target_offset = self.latest_streamed_offset.load(Ordering::Acquire);

            // Phase 2: wait for enough replicas to durably cover `target_offset`.
            loop {
                let notified = self.barrier_notify.notified();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let ack_count = self
                    .replicas
                    .iter()
                    .filter(|r| r.last_acked_offset.load(Ordering::Acquire) >= target_offset)
                    .count();
                if ack_count >= required_acks {
                    return target_offset;
                }
                notified.await;
            }
        })
        .await;

        match waited {
            Ok(_target) => Ok(()),
            Err(_) => {
                if let Some(ref m) = self.metrics {
                    m.replication_sync_timeouts_total.inc();
                }
                Err(format!(
                    "sync replication timeout after {}ms (head_sequence={}, need {} ACKs)",
                    self.config.sync_timeout_ms, head_sequence, self.config.sync_replicas
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

    /// Highest WAL offset the tailer/catch-up has streamed to replicas. Exposed
    /// for observability and tests that need to know a specific write has been
    /// fanned out (its offset now contributes to the sync barrier target).
    pub fn latest_streamed_offset(&self) -> u64 {
        self.latest_streamed_offset.load(Ordering::Acquire)
    }

    /// Highest WAL sequence the tailer/catch-up has streamed to replicas.
    pub fn latest_streamed_sequence(&self) -> u64 {
        self.latest_streamed_sequence.load(Ordering::Acquire)
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
