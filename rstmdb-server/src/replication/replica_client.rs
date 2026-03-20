//! Replica-side replication client.
//!
//! Connects to a primary server, receives WAL entry stream, and applies entries locally.

use crate::replication::protocol::ReplicationMessage;
use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::{Decoder, Encoder};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Replica-side client that connects to the primary and applies streamed entries.
pub struct ReplicaClient {
    engine: Arc<StateMachineEngine>,
    upstream: String,
    auth_token: Option<String>,
    /// Last sequence successfully applied.
    last_applied_sequence: Arc<AtomicU64>,
    /// Current primary sequence (from heartbeats).
    primary_sequence: Arc<AtomicU64>,
}

impl ReplicaClient {
    /// Creates a new replica client.
    pub fn new(
        engine: Arc<StateMachineEngine>,
        upstream: String,
        auth_token: Option<String>,
    ) -> Self {
        // Determine last applied sequence from WAL
        let last_seq = engine.wal().next_sequence().saturating_sub(1);

        Self {
            engine,
            upstream,
            auth_token,
            last_applied_sequence: Arc::new(AtomicU64::new(last_seq)),
            primary_sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Runs the replication client loop with automatic reconnection.
    pub async fn run(&self, mut shutdown: tokio::sync::broadcast::Receiver<()>) {
        loop {
            tokio::select! {
                _ = self.connect_and_stream() => {
                    tracing::warn!("Replication connection lost, reconnecting in 2s...");
                    tokio::time::sleep(Duration::from_secs(2)).await;
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

        let mut stream = TcpStream::connect(&self.upstream).await?;
        tracing::info!("Connected to primary");

        // Send auth handshake
        let last_seq = self.last_applied_sequence.load(Ordering::Acquire);
        let auth_msg = ReplicationMessage::ReplicateAuth {
            auth_token: self.auth_token.clone(),
            last_sequence: last_seq,
        };
        Self::send_message(&mut stream, &auth_msg).await?;

        // Read sync response
        let mut decoder = Decoder::new();
        let mut buf = [0u8; 8192];

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
                        offset: _,
                        entry,
                    } => {
                        // Apply the entry
                        match self.engine.apply_replicated_entry(entry) {
                            Ok((local_seq, _local_offset)) => {
                                self.last_applied_sequence
                                    .store(local_seq, Ordering::Release);

                                tracing::info!(
                                    "Applied replicated entry: primary_seq={}, local_seq={}",
                                    sequence,
                                    local_seq
                                );

                                // Send ACK
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
                    } => {
                        tracing::debug!(
                            "Heartbeat from primary: sequence={}",
                            primary_sequence
                        );
                        self.primary_sequence
                            .store(primary_sequence, Ordering::Release);
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
        stream: &mut TcpStream,
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
}
