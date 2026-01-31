//! Connection management.

use crate::error::ClientError;
use crate::stream::ClientStream;
use crate::tls::{create_insecure_tls_connector, create_tls_connector};
use rstmdb_protocol::message::*;
use rstmdb_protocol::{Decoder, Encoder, PROTOCOL_VERSION};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, oneshot, Mutex};

/// Default read buffer size (8 KiB).
pub const DEFAULT_READ_BUFFER_SIZE: usize = 8 * 1024;

/// Minimum read buffer size (1 KiB).
pub const MIN_READ_BUFFER_SIZE: usize = 1024;

/// Maximum read buffer size (1 MiB).
pub const MAX_READ_BUFFER_SIZE: usize = 1024 * 1024;

/// TLS configuration for client connections.
#[derive(Debug, Clone, Default)]
pub struct TlsClientConfig {
    /// Enable TLS for the connection.
    pub enabled: bool,
    /// Path to PEM-encoded CA certificate(s) for server verification.
    /// If None, system roots are used.
    pub ca_cert_path: Option<PathBuf>,
    /// Path to PEM-encoded client certificate (for mTLS).
    pub client_cert_path: Option<PathBuf>,
    /// Path to PEM-encoded client private key (for mTLS).
    pub client_key_path: Option<PathBuf>,
    /// Skip server certificate verification (INSECURE - development only).
    pub insecure: bool,
    /// Server name for SNI (defaults to hostname from address).
    pub server_name: Option<String>,
}

impl TlsClientConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_ca_cert(mut self, path: impl Into<PathBuf>) -> Self {
        self.ca_cert_path = Some(path.into());
        self.enabled = true;
        self
    }

    pub fn with_client_cert(
        mut self,
        cert_path: impl Into<PathBuf>,
        key_path: impl Into<PathBuf>,
    ) -> Self {
        self.client_cert_path = Some(cert_path.into());
        self.client_key_path = Some(key_path.into());
        self.enabled = true;
        self
    }

    pub fn with_insecure(mut self) -> Self {
        self.insecure = true;
        self.enabled = true;
        self
    }

    pub fn with_server_name(mut self, name: impl Into<String>) -> Self {
        self.server_name = Some(name.into());
        self
    }
}

/// Connection configuration.
#[derive(Debug, Clone)]
pub struct ConnectionConfig {
    /// Server address.
    pub addr: SocketAddr,
    /// Connection timeout.
    pub connect_timeout: Duration,
    /// Request timeout.
    pub request_timeout: Duration,
    /// Client name for HELLO.
    pub client_name: Option<String>,
    /// Read buffer size for socket reads.
    pub read_buffer_size: usize,
    /// Authentication token (optional).
    pub auth_token: Option<String>,
    /// TLS configuration (optional).
    pub tls: Option<TlsClientConfig>,
}

impl ConnectionConfig {
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            connect_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
            client_name: None,
            read_buffer_size: DEFAULT_READ_BUFFER_SIZE,
            auth_token: None,
            tls: None,
        }
    }

    pub fn with_client_name(mut self, name: impl Into<String>) -> Self {
        self.client_name = Some(name.into());
        self
    }

    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    pub fn with_request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = timeout;
        self
    }

    pub fn with_read_buffer_size(mut self, size: usize) -> Self {
        self.read_buffer_size = size.clamp(MIN_READ_BUFFER_SIZE, MAX_READ_BUFFER_SIZE);
        self
    }

    pub fn with_auth_token(mut self, token: impl Into<String>) -> Self {
        self.auth_token = Some(token.into());
        self
    }

    pub fn with_tls(mut self, tls_config: TlsClientConfig) -> Self {
        self.tls = Some(tls_config);
        self
    }
}

/// Default capacity for stream event channel.
const STREAM_EVENT_CHANNEL_CAPACITY: usize = 256;

/// A connection to an rstmdb server.
pub struct Connection {
    config: ConnectionConfig,
    /// Write half of the stream (for sending requests).
    writer: Mutex<Option<WriteHalf<ClientStream>>>,
    /// Read half of the stream (for receiving responses).
    reader: Mutex<Option<ReadHalf<ClientStream>>>,
    /// Decoder for parsing responses.
    decoder: Mutex<Decoder>,
    /// Pending requests waiting for responses.
    pending: Mutex<HashMap<String, oneshot::Sender<Response>>>,
    /// Next request ID.
    next_id: AtomicU64,
    /// Is the connection established?
    connected: std::sync::atomic::AtomicBool,
    /// Broadcast channel for stream events.
    stream_events: broadcast::Sender<StreamEvent>,
}

impl Connection {
    /// Creates a new connection (not yet connected).
    pub fn new(config: ConnectionConfig) -> Self {
        let (stream_events, _) = broadcast::channel(STREAM_EVENT_CHANNEL_CAPACITY);
        Self {
            config,
            writer: Mutex::new(None),
            reader: Mutex::new(None),
            decoder: Mutex::new(Decoder::new()),
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            connected: std::sync::atomic::AtomicBool::new(false),
            stream_events,
        }
    }

    /// Subscribes to stream events (for watch commands).
    /// Returns a receiver that will receive StreamEvent messages.
    pub fn subscribe_stream_events(&self) -> broadcast::Receiver<StreamEvent> {
        self.stream_events.subscribe()
    }

    /// Connects to the server.
    pub async fn connect(&self) -> Result<(), ClientError> {
        tracing::debug!("Connecting to {}...", self.config.addr);

        let tcp_stream = tokio::time::timeout(
            self.config.connect_timeout,
            TcpStream::connect(self.config.addr),
        )
        .await
        .map_err(|_| {
            tracing::debug!("Connection timeout");
            ClientError::Timeout
        })?
        .map_err(|e| {
            tracing::debug!("Connection failed: {}", e);
            ClientError::Io(e)
        })?;

        tracing::debug!("TCP connected, configuring socket");

        // Configure TCP options for better performance
        tcp_stream.set_nodelay(true).ok();

        // Upgrade to TLS if configured
        let stream = if let Some(ref tls_config) = self.config.tls {
            if tls_config.enabled {
                let host = self.config.addr.ip().to_string();
                let (connector, server_name) = if tls_config.insecure {
                    tracing::warn!("Using insecure TLS (certificate verification disabled)");
                    create_insecure_tls_connector(tls_config, &host)?
                } else {
                    create_tls_connector(tls_config, &host)?
                };

                tracing::debug!("Performing TLS handshake...");
                let tls_stream = connector
                    .connect(server_name, tcp_stream)
                    .await
                    .map_err(|e| ClientError::TlsHandshake(e.to_string()))?;

                tracing::debug!("TLS handshake complete");
                ClientStream::Tls { stream: tls_stream }
            } else {
                ClientStream::Plain { stream: tcp_stream }
            }
        } else {
            ClientStream::Plain { stream: tcp_stream }
        };

        // Split into read/write halves for concurrent access
        let (read_half, write_half) = tokio::io::split(stream);
        *self.writer.lock().await = Some(write_half);
        *self.reader.lock().await = Some(read_half);
        self.decoder.lock().await.clear();

        // Perform handshake (reads response directly, before read_loop is running)
        tracing::debug!("Starting protocol handshake...");
        self.handshake().await?;
        tracing::debug!("Handshake complete");

        // Mark as connected only after successful handshake
        self.connected.store(true, Ordering::SeqCst);

        Ok(())
    }

    /// Performs the HELLO handshake and optional authentication.
    /// This reads the response directly from the stream since read_loop isn't running yet.
    async fn handshake(&self) -> Result<(), ClientError> {
        // Send HELLO
        let hello = HelloParams {
            protocol_version: PROTOCOL_VERSION,
            client_name: self.config.client_name.clone(),
            wire_modes: vec!["binary_json".to_string()],
            features: vec!["idempotency".to_string(), "batch".to_string()],
        };

        let id = self.next_id.fetch_add(1, Ordering::SeqCst).to_string();
        let request = Request::new(&id, Operation::Hello).with_params(serde_json::to_value(hello)?);

        // Encode and send
        let encoded = Encoder::encode_request(&request)?;
        tracing::debug!("Sending HELLO request ({} bytes)", encoded.len());
        {
            let mut writer_guard = self.writer.lock().await;
            let writer = writer_guard.as_mut().ok_or(ClientError::NotConnected)?;
            writer.write_all(&encoded).await.map_err(ClientError::Io)?;
        }
        tracing::debug!("HELLO sent, waiting for response...");

        // Read response directly (since read_loop isn't running yet)
        let response = self.read_single_response().await?;
        tracing::debug!("HELLO response received: ok={}", response.is_ok());

        if response.is_error() {
            let err = response.error.unwrap();
            return Err(ClientError::ServerError {
                code: err.code,
                message: err.message,
                retryable: err.retryable,
            });
        }

        // Auto-authenticate if token is configured
        if let Some(ref token) = self.config.auth_token {
            tracing::debug!("Authenticating with server...");
            self.authenticate_internal(token).await?;
            tracing::debug!("Authentication successful");
        }

        Ok(())
    }

    /// Authenticates with the server (internal, during handshake).
    async fn authenticate_internal(&self, token: &str) -> Result<(), ClientError> {
        let auth = AuthParams {
            method: "bearer".to_string(),
            token: token.to_string(),
        };

        let id = self.next_id.fetch_add(1, Ordering::SeqCst).to_string();
        let request = Request::new(&id, Operation::Auth).with_params(serde_json::to_value(auth)?);

        // Encode and send
        let encoded = Encoder::encode_request(&request)?;
        {
            let mut writer_guard = self.writer.lock().await;
            let writer = writer_guard.as_mut().ok_or(ClientError::NotConnected)?;
            writer.write_all(&encoded).await.map_err(ClientError::Io)?;
        }

        // Read response
        let response = self.read_single_response().await?;

        if response.is_error() {
            let err = response.error.unwrap();
            return Err(ClientError::ServerError {
                code: err.code,
                message: err.message,
                retryable: false,
            });
        }

        Ok(())
    }

    /// Reads a single response from the stream with timeout.
    /// Used during handshake before read_loop is started.
    async fn read_single_response(&self) -> Result<Response, ClientError> {
        let buffer_size = self.config.read_buffer_size;
        let timeout = self.config.request_timeout;

        tokio::time::timeout(timeout, async {
            let mut buf = vec![0u8; buffer_size];

            loop {
                tracing::debug!("Waiting to read from socket...");
                let n = {
                    let mut reader_guard = self.reader.lock().await;
                    let reader = reader_guard.as_mut().ok_or(ClientError::NotConnected)?;
                    reader.read(&mut buf).await.map_err(ClientError::Io)?
                };

                tracing::debug!("Read {} bytes from socket", n);

                if n == 0 {
                    tracing::debug!("Connection closed (0 bytes)");
                    return Err(ClientError::ConnectionClosed);
                }

                self.decoder.lock().await.extend(&buf[..n]);
                let buffered = self.decoder.lock().await.buffered();
                tracing::debug!("Decoder buffer now has {} bytes", buffered);

                if let Some(response) = self.decoder.lock().await.decode_response()? {
                    tracing::debug!("Decoded response id={}", response.id);
                    return Ok(response);
                }
                tracing::debug!("No complete response yet, continuing to read...");
            }
        })
        .await
        .map_err(|_| {
            tracing::debug!("Read timeout");
            ClientError::Timeout
        })?
    }

    /// Sends a request and waits for response.
    pub async fn request(
        &self,
        op: Operation,
        params: serde_json::Value,
    ) -> Result<Response, ClientError> {
        if !self.connected.load(Ordering::SeqCst) {
            tracing::debug!("request() called but not connected");
            return Err(ClientError::NotConnected);
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst).to_string();
        tracing::debug!("Sending request id={} op={:?}", id, op);
        let request = Request::new(&id, op).with_params(params);

        // Encode request
        let encoded = Encoder::encode_request(&request)?;

        // Create response channel
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);

        // Send request
        {
            let mut writer_guard = self.writer.lock().await;
            let writer = writer_guard.as_mut().ok_or(ClientError::NotConnected)?;
            writer.write_all(&encoded).await.map_err(ClientError::Io)?;
        }
        tracing::debug!(
            "Request id={} sent ({} bytes), waiting for response...",
            id,
            encoded.len()
        );

        // Wait for response with timeout
        let response = tokio::time::timeout(self.config.request_timeout, rx)
            .await
            .map_err(|_| {
                tracing::debug!("Request id={} timed out", id);
                // Remove pending request on timeout
                let _ = self.pending.try_lock().map(|mut p| p.remove(&id));
                ClientError::Timeout
            })?
            .map_err(|_| {
                tracing::debug!("Request id={} channel closed", id);
                ClientError::ConnectionClosed
            })?;

        tracing::debug!("Request id={} got response", id);
        Ok(response)
    }

    /// Reads and dispatches responses and stream events (call this in a background task).
    pub async fn read_loop(&self) -> Result<(), ClientError> {
        tracing::debug!("read_loop started");
        let buffer_size = self.config.read_buffer_size;
        let mut buf = vec![0u8; buffer_size];

        loop {
            tracing::debug!("read_loop: waiting for data...");
            let n = {
                let mut reader_guard = self.reader.lock().await;
                let reader = reader_guard.as_mut().ok_or(ClientError::NotConnected)?;
                reader.read(&mut buf).await.map_err(ClientError::Io)?
            };

            tracing::debug!("read_loop: received {} bytes", n);

            if n == 0 {
                tracing::debug!("read_loop: connection closed");
                self.connected.store(false, Ordering::SeqCst);
                return Err(ClientError::ConnectionClosed);
            }

            self.decoder.lock().await.extend(&buf[..n]);

            // Process complete frames - could be responses or stream events
            loop {
                let mut decoder = self.decoder.lock().await;
                if let Some(frame) = decoder.decode_frame()? {
                    let payload = std::str::from_utf8(&frame.payload)
                        .map_err(|_| rstmdb_protocol::ProtocolError::InvalidUtf8)?;

                    // Check message type to distinguish between responses and stream events
                    let msg: serde_json::Value = serde_json::from_str(payload)?;
                    let msg_type = msg["type"].as_str().unwrap_or("");

                    match msg_type {
                        "response" => {
                            let response: Response = serde_json::from_value(msg)?;
                            let id = response.id.clone();
                            tracing::debug!("read_loop: dispatching response id={}", id);
                            drop(decoder); // Release lock before sending
                            if let Some(tx) = self.pending.lock().await.remove(&id) {
                                let _ = tx.send(response);
                            } else {
                                tracing::debug!("read_loop: no pending request for id={}", id);
                            }
                        }
                        "event" => {
                            let event: StreamEvent = serde_json::from_value(msg)?;
                            tracing::debug!(
                                "read_loop: dispatching stream event sub_id={}",
                                event.subscription_id
                            );
                            drop(decoder); // Release lock before sending
                                           // Broadcast to all subscribers - ignore errors (no receivers)
                            let _ = self.stream_events.send(event);
                        }
                        _ => {
                            tracing::warn!("read_loop: unknown message type: {}", msg_type);
                            drop(decoder);
                        }
                    }
                } else {
                    break;
                }
            }
        }
    }

    /// Returns whether the connection is established.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    /// Closes the connection.
    pub async fn close(&self) -> Result<(), ClientError> {
        tracing::debug!("Closing connection...");

        // Mark as disconnected first to stop any new requests
        self.connected.store(false, Ordering::SeqCst);

        // Close the writer
        if let Some(mut writer) = self.writer.lock().await.take() {
            tracing::debug!("Shutting down writer");
            let _ = writer.shutdown().await;
        }

        // The reader will get EOF when writer is closed
        let _ = self.reader.lock().await.take();

        // Cancel any pending requests
        let mut pending = self.pending.lock().await;
        tracing::debug!("Clearing {} pending requests", pending.len());
        pending.clear();

        tracing::debug!("Connection closed");
        Ok(())
    }

    /// Returns the number of pending requests.
    pub fn pending_count(&self) -> usize {
        self.pending.try_lock().map(|p| p.len()).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_defaults() {
        let config = ConnectionConfig::new("127.0.0.1:7401".parse().unwrap());
        assert_eq!(config.read_buffer_size, DEFAULT_READ_BUFFER_SIZE);
        assert_eq!(config.connect_timeout, Duration::from_secs(10));
        assert_eq!(config.request_timeout, Duration::from_secs(30));
    }

    #[test]
    fn test_config_buffer_clamping() {
        let config =
            ConnectionConfig::new("127.0.0.1:7401".parse().unwrap()).with_read_buffer_size(100); // Below minimum
        assert_eq!(config.read_buffer_size, MIN_READ_BUFFER_SIZE);

        let config = ConnectionConfig::new("127.0.0.1:7401".parse().unwrap())
            .with_read_buffer_size(10 * 1024 * 1024); // Above maximum
        assert_eq!(config.read_buffer_size, MAX_READ_BUFFER_SIZE);
    }
}
