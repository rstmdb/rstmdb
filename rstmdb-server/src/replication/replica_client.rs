//! Replica-side replication client.
//!
//! Connects to a primary server, receives WAL entry stream, and applies entries locally.
//! Supports optional TLS and uses exponential backoff with jitter for reconnects.

use crate::config::ReplicationConfig;
use crate::replication::protocol::ReplicationMessage;
use crate::tls::create_replication_tls_connector;
use pin_project_lite::pin_project;
use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::{Decoder, Encoder};
use rustls::pki_types::ServerName;
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;

pin_project! {
    /// Client-side stream that can be plain TCP or TLS.
    #[project = ReplicaStreamProj]
    enum ReplicaStream {
        Plain { #[pin] stream: TcpStream },
        Tls { #[pin] stream: Box<TlsStream<TcpStream>> },
    }
}

impl AsyncRead for ReplicaStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.project() {
            ReplicaStreamProj::Plain { stream } => stream.poll_read(cx, buf),
            ReplicaStreamProj::Tls { stream } => stream.poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ReplicaStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.project() {
            ReplicaStreamProj::Plain { stream } => stream.poll_write(cx, buf),
            ReplicaStreamProj::Tls { stream } => stream.poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            ReplicaStreamProj::Plain { stream } => stream.poll_flush(cx),
            ReplicaStreamProj::Tls { stream } => stream.poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.project() {
            ReplicaStreamProj::Plain { stream } => stream.poll_shutdown(cx),
            ReplicaStreamProj::Tls { stream } => stream.poll_shutdown(cx),
        }
    }
}

/// Replica-side client that connects to the primary and applies streamed entries.
pub struct ReplicaClient {
    config: ReplicationConfig,
    engine: Arc<StateMachineEngine>,
    upstream: String,
    auth_token: Option<String>,
    /// Pre-built TLS connector (if TLS is enabled).
    tls: Option<(TlsConnector, ServerName<'static>)>,
    /// Last local sequence successfully applied.
    last_applied_sequence: Arc<AtomicU64>,
    /// Highest **primary** WAL offset we've applied. Sent to the primary on
    /// reconnect so its catchup can filter by offset (monotonic on disk)
    /// instead of sequence (which can be non-monotonic under concurrent writes).
    last_applied_primary_offset: Arc<AtomicU64>,
    /// Current primary sequence (from heartbeats).
    primary_sequence: Arc<AtomicU64>,
    /// Timestamp (Unix ms) of the last entry we applied (from the primary).
    last_applied_ts_ms: Arc<AtomicU64>,
    /// Timestamp (Unix ms) of the primary's most recent write (from heartbeats).
    primary_latest_write_ts_ms: Arc<AtomicU64>,
    /// Number of consecutive failed reconnect attempts (for backoff).
    reconnect_attempts: Arc<AtomicU32>,
}

impl ReplicaClient {
    /// Creates a new replica client.
    pub fn new(
        config: ReplicationConfig,
        engine: Arc<StateMachineEngine>,
        upstream: String,
        auth_token: Option<String>,
    ) -> Result<Self, String> {
        let last_seq = engine.wal().next_sequence().saturating_sub(1);

        // Derive the server host for SNI from `upstream` (strip port).
        let server_host = upstream
            .rsplit_once(':')
            .map(|(h, _)| h)
            .unwrap_or(upstream.as_str());

        let tls = if config.tls_enabled {
            let connector = create_replication_tls_connector(&config, server_host)
                .map_err(|e| format!("failed to build replication TLS connector: {}", e))?;
            Some(connector)
        } else {
            None
        };

        Ok(Self {
            config,
            engine,
            upstream,
            auth_token,
            tls,
            last_applied_sequence: Arc::new(AtomicU64::new(last_seq)),
            last_applied_primary_offset: Arc::new(AtomicU64::new(0)),
            primary_sequence: Arc::new(AtomicU64::new(0)),
            last_applied_ts_ms: Arc::new(AtomicU64::new(0)),
            primary_latest_write_ts_ms: Arc::new(AtomicU64::new(0)),
            reconnect_attempts: Arc::new(AtomicU32::new(0)),
        })
    }

    /// Runs the replication client loop with automatic reconnection.
    pub async fn run(&self, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        loop {
            tokio::select! {
                _ = self.connect_and_stream() => {
                    let attempts = self.reconnect_attempts.fetch_add(1, Ordering::AcqRel) + 1;
                    let delay = backoff_with_jitter(
                        self.config.reconnect_delay(),
                        self.config.reconnect_max_delay(),
                        attempts,
                    );
                    tracing::warn!(
                        "Replication connection lost (attempt #{}), reconnecting in {:?}",
                        attempts, delay
                    );
                    tokio::time::sleep(delay).await;
                }
                _ = shutdown.recv() => {
                    tracing::info!("Replica client shutting down");
                    return;
                }
            }
        }
    }

    /// Connects to the primary and streams entries.
    async fn connect_and_stream(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        tracing::info!("Connecting to primary at {}", self.upstream);

        let tcp = TcpStream::connect(&self.upstream).await?;
        let mut stream = if let Some((connector, server_name)) = &self.tls {
            tracing::debug!("Upgrading to TLS");
            let tls_stream = connector.connect(server_name.clone(), tcp).await?;
            ReplicaStream::Tls {
                stream: Box::new(tls_stream),
            }
        } else {
            ReplicaStream::Plain { stream: tcp }
        };
        tracing::info!("Connected to primary");

        // Send auth handshake. We send BOTH last_sequence (replica's local
        // sequence, legacy) and last_primary_offset (highest primary offset
        // we've applied). The primary prefers the offset filter.
        let last_seq = self.last_applied_sequence.load(Ordering::Acquire);
        let last_primary_offset = self.last_applied_primary_offset.load(Ordering::Acquire);
        let auth_msg = ReplicationMessage::ReplicateAuth {
            auth_token: self.auth_token.clone(),
            last_sequence: last_seq,
            last_primary_offset,
        };
        Self::send_message(&mut stream, &auth_msg).await?;

        // Read sync response
        let mut decoder = Decoder::new();
        let mut buf = [0u8; super::REPLICATION_READ_BUF_SIZE];

        let sync_resp = loop {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                return Err("connection closed during handshake".into());
            }
            decoder.extend(&buf[..n]);
            if let Some(payload) = decoder.decode_raw()? {
                break ReplicationMessage::from_bytes(&payload)?;
            }
        };

        match sync_resp {
            ReplicationMessage::ReplicateSyncResponse {
                ok,
                primary_sequence,
                error,
            } => {
                if !ok {
                    return Err(format!(
                        "primary rejected connection: {}",
                        error.unwrap_or_default()
                    )
                    .into());
                }
                self.primary_sequence
                    .store(primary_sequence, Ordering::Release);
                tracing::info!(
                    "Handshake complete: primary_sequence={}, our_last_sequence={}",
                    primary_sequence,
                    last_seq
                );
                // Successful handshake — reset backoff counter
                self.reconnect_attempts.store(0, Ordering::Release);
            }
            _ => return Err("unexpected handshake response".into()),
        }

        // Stream entries
        loop {
            let n = stream.read(&mut buf).await?;
            if n == 0 {
                return Err("connection closed by primary".into());
            }
            tracing::debug!("Received {} bytes from primary", n);
            decoder.extend(&buf[..n]);

            while let Some(payload) = decoder.decode_raw()? {
                let msg = ReplicationMessage::from_bytes(&payload)?;
                match msg {
                    ReplicationMessage::ReplicateEntry {
                        sequence,
                        offset,
                        entry,
                        timestamp_ms,
                    } => {
                        // Idempotency: if we've already applied this primary
                        // offset, skip. Catch-up and live streaming can
                        // legitimately overlap (primary catchup reads the
                        // whole WAL while the live tailer also fans out from
                        // a stale pos.next_offset). Without this dedup, the
                        // replica's WAL would double each overlapping entry.
                        let already_applied =
                            self.last_applied_primary_offset.load(Ordering::Acquire);
                        if offset != 0 && offset <= already_applied {
                            // Still ACK so the primary's sync barrier (if any)
                            // can resolve.
                            let ack = ReplicationMessage::ReplicateAck { sequence };
                            Self::send_message(&mut stream, &ack).await?;
                            tracing::debug!(
                                "Skipped duplicate replicated entry: primary_offset={} already_applied={}",
                                offset, already_applied,
                            );
                            continue;
                        }

                        // Apply the entry, passing the primary's offset so the
                        // replica's in-memory state reflects primary offsets.
                        match self.engine.apply_replicated_entry(offset, entry) {
                            Ok((local_seq, _local_offset)) => {
                                self.last_applied_sequence
                                    .store(local_seq, Ordering::Release);
                                // Track highest primary offset — used on
                                // reconnect to drive offset-based catchup AND
                                // for in-line dedup above.
                                self.last_applied_primary_offset
                                    .fetch_max(offset, Ordering::Release);
                                if timestamp_ms > 0 {
                                    self.last_applied_ts_ms
                                        .store(timestamp_ms, Ordering::Release);
                                }

                                tracing::info!(
                                    "Applied replicated entry: primary_seq={}, local_seq={}",
                                    sequence,
                                    local_seq
                                );

                                let ack = ReplicationMessage::ReplicateAck { sequence };
                                Self::send_message(&mut stream, &ack).await?;
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Failed to apply replicated entry seq={}: {}",
                                    sequence,
                                    e
                                );
                            }
                        }
                    }

                    ReplicationMessage::ReplicateHeartbeat {
                        primary_sequence,
                        timestamp_ms: _,
                        primary_latest_write_ts_ms,
                    } => {
                        tracing::debug!("Heartbeat from primary: sequence={}", primary_sequence);
                        self.primary_sequence
                            .store(primary_sequence, Ordering::Release);
                        if primary_latest_write_ts_ms > 0 {
                            self.primary_latest_write_ts_ms
                                .store(primary_latest_write_ts_ms, Ordering::Release);
                        }
                    }

                    _ => {
                        tracing::debug!("Unexpected message from primary");
                    }
                }
            }
        }
    }

    /// Sends a single replication message to the primary.
    async fn send_message(
        stream: &mut ReplicaStream,
        msg: &ReplicationMessage,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        let bytes = serde_json::to_vec(msg)?;
        let frame = Encoder::encode_raw(&bytes);
        stream.write_all(&frame).await?;
        Ok(())
    }

    /// Returns the last applied sequence number.
    pub fn last_applied_sequence(&self) -> u64 {
        self.last_applied_sequence.load(Ordering::Acquire)
    }

    /// Returns the current primary sequence number (from heartbeats).
    pub fn primary_sequence(&self) -> u64 {
        self.primary_sequence.load(Ordering::Acquire)
    }

    /// Returns the replication lag in entries.
    pub fn lag_entries(&self) -> u64 {
        let primary = self.primary_sequence.load(Ordering::Acquire);
        let local = self.last_applied_sequence.load(Ordering::Acquire);
        primary.saturating_sub(local)
    }

    /// Returns the replication lag in seconds, computed as the difference
    /// between the primary's most recent write timestamp (from heartbeats)
    /// and the timestamp of the last entry we applied.
    ///
    /// Returns 0 if we're fully caught up, if no heartbeat has been received
    /// yet, or if we haven't applied any entries (so no basis for comparison).
    pub fn lag_seconds(&self) -> f64 {
        let primary_ts = self.primary_latest_write_ts_ms.load(Ordering::Acquire);
        let applied_ts = self.last_applied_ts_ms.load(Ordering::Acquire);

        // No data yet to compute time-based lag
        if primary_ts == 0 || applied_ts == 0 {
            return 0.0;
        }

        // If caught up by sequence, lag is 0 regardless of timestamps
        if self.lag_entries() == 0 {
            return 0.0;
        }

        primary_ts.saturating_sub(applied_ts) as f64 / 1000.0
    }
}

/// Computes a capped exponential backoff with full jitter.
///
/// The delay doubles with each attempt up to `max`, then full jitter picks a
/// uniform random value in `[0, capped]`. This is the "full jitter" strategy
/// recommended by AWS for avoiding thundering-herd reconnects.
fn backoff_with_jitter(base: Duration, max: Duration, attempt: u32) -> Duration {
    // Cap exponent to avoid overflow on u64
    let exp = attempt.min(30);
    let factor = 1u64 << exp; // 2^exp
    let base_ms = base.as_millis() as u64;
    let max_ms = max.as_millis() as u64;

    let capped = base_ms.saturating_mul(factor).min(max_ms);

    // Full jitter using a simple hash-based PRNG seeded by current nanos.
    let jittered = if capped == 0 {
        0
    } else {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        // Splitmix64-style mixing
        let mut x = seed ^ (attempt as u64).wrapping_mul(0x9E3779B97F4A7C15);
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
        x ^= x >> 31;
        x % (capped + 1)
    };

    Duration::from_millis(jittered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_is_bounded() {
        let base = Duration::from_millis(100);
        let max = Duration::from_secs(10);

        for attempt in 1..20 {
            let delay = backoff_with_jitter(base, max, attempt);
            assert!(
                delay <= max,
                "attempt {}: delay {:?} > max {:?}",
                attempt,
                delay,
                max
            );
        }
    }

    #[test]
    fn test_backoff_grows_then_caps() {
        let base = Duration::from_millis(100);
        let max = Duration::from_secs(10);

        // With full jitter, delay is in [0, capped]. The *maximum possible*
        // capped value grows exponentially then saturates at `max`.
        // After attempt ~7 (100ms * 2^7 = 12800ms > 10s), capped == max.
        // We sample many times and check the max observed is plausible.
        let mut max_observed_low = Duration::ZERO;
        let mut max_observed_high = Duration::ZERO;
        for _ in 0..200 {
            let d1 = backoff_with_jitter(base, max, 1);
            let d10 = backoff_with_jitter(base, max, 10);
            max_observed_low = max_observed_low.max(d1);
            max_observed_high = max_observed_high.max(d10);
        }

        // Attempt 1: capped at 200ms, so max observed should be <= 200ms
        assert!(max_observed_low <= Duration::from_millis(200));
        // Attempt 10: should reach closer to max
        assert!(max_observed_high > Duration::from_secs(1));
    }

    #[test]
    fn test_backoff_never_panics_on_large_attempt() {
        let base = Duration::from_secs(1);
        let max = Duration::from_secs(60);
        // Very large attempt number must not overflow
        let delay = backoff_with_jitter(base, max, u32::MAX);
        assert!(delay <= max);
    }
}
