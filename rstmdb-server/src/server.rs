//! TCP server implementation.

use crate::broadcast::{EventBroadcaster, EventFilter, InstanceEvent};
use crate::config::AuthConfig;
use crate::error::ServerError;
use crate::handler::CommandHandler;
use crate::metrics::Metrics;
use crate::session::{Session, SessionState, WireMode};
use crate::stream::MaybeTlsStream;
use bytes::BytesMut;
use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::message::{Operation, StreamEvent};
use rstmdb_protocol::{Decoder, Encoder};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{broadcast, mpsc};
use tokio_rustls::TlsAcceptor;

/// Server configuration.
#[derive(Clone)]
pub struct ServerConfig {
    /// Address to bind to.
    pub bind_addr: SocketAddr,
    /// Idle connection timeout.
    pub idle_timeout: Duration,
    /// Whether authentication is required.
    pub auth_required: bool,
    /// Maximum concurrent connections.
    pub max_connections: usize,
    /// Maximum number of versions per machine (0 = unlimited).
    pub max_machine_versions: u32,
    /// TLS acceptor (if TLS is enabled).
    pub tls_acceptor: Option<Arc<TlsAcceptor>>,
    /// Metrics instance (if metrics are enabled).
    pub metrics: Option<Arc<Metrics>>,
}

impl std::fmt::Debug for ServerConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServerConfig")
            .field("bind_addr", &self.bind_addr)
            .field("idle_timeout", &self.idle_timeout)
            .field("auth_required", &self.auth_required)
            .field("max_connections", &self.max_connections)
            .field("max_machine_versions", &self.max_machine_versions)
            .field("tls_enabled", &self.tls_acceptor.is_some())
            .field("metrics_enabled", &self.metrics.is_some())
            .finish()
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "127.0.0.1:7401".parse().unwrap(),
            idle_timeout: Duration::from_secs(300),
            auth_required: false,
            max_connections: 1000,
            max_machine_versions: 0, // unlimited
            tls_acceptor: None,
            metrics: None,
        }
    }
}

impl ServerConfig {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            bind_addr,
            ..Default::default()
        }
    }

    /// Sets the TLS acceptor.
    pub fn with_tls(mut self, acceptor: TlsAcceptor) -> Self {
        self.tls_acceptor = Some(Arc::new(acceptor));
        self
    }

    /// Sets the metrics instance.
    pub fn with_metrics(mut self, metrics: Arc<Metrics>) -> Self {
        self.metrics = Some(metrics);
        self
    }

    /// Returns whether TLS is enabled.
    pub fn tls_enabled(&self) -> bool {
        self.tls_acceptor.is_some()
    }

    /// Returns whether metrics are enabled.
    pub fn metrics_enabled(&self) -> bool {
        self.metrics.is_some()
    }
}

/// Server statistics.
#[derive(Debug, Default)]
pub struct ServerStats {
    pub connections_total: AtomicU64,
    pub connections_active: AtomicU64,
    pub requests_total: AtomicU64,
    pub errors_total: AtomicU64,
}

/// TCP server for rstmdb.
pub struct Server {
    config: ServerConfig,
    handler: Arc<CommandHandler>,
    broadcaster: Arc<EventBroadcaster>,
    stats: Arc<ServerStats>,
    shutdown: broadcast::Sender<()>,
    running: AtomicBool,
}

/// Default broadcast channel capacity.
const DEFAULT_BROADCAST_CAPACITY: usize = 1024;

/// Event forwarded from a subscription task to the connection handler.
struct ForwardedEvent {
    subscription_id: String,
    event: InstanceEvent,
    include_ctx: bool,
}

impl Server {
    /// Creates a new server.
    pub fn new(config: ServerConfig, engine: Arc<StateMachineEngine>) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        let broadcaster = Arc::new(EventBroadcaster::new(DEFAULT_BROADCAST_CAPACITY));
        let mut handler = CommandHandler::new(engine)
            .with_broadcaster(broadcaster.clone())
            .with_max_machine_versions(config.max_machine_versions);
        if let Some(ref metrics) = config.metrics {
            handler = handler.with_metrics(metrics.clone());
        }
        Self {
            config,
            handler: Arc::new(handler),
            broadcaster,
            stats: Arc::new(ServerStats::default()),
            shutdown: shutdown_tx,
            running: AtomicBool::new(false),
        }
    }

    /// Creates a new server with authentication.
    pub fn with_auth(
        config: ServerConfig,
        engine: Arc<StateMachineEngine>,
        auth_config: &AuthConfig,
    ) -> Self {
        let (shutdown_tx, _) = broadcast::channel(1);
        let broadcaster = Arc::new(EventBroadcaster::new(DEFAULT_BROADCAST_CAPACITY));
        let mut handler = CommandHandler::with_auth(engine, auth_config)
            .with_broadcaster(broadcaster.clone())
            .with_max_machine_versions(config.max_machine_versions);
        if let Some(ref metrics) = config.metrics {
            handler = handler.with_metrics(metrics.clone());
        }
        Self {
            config,
            handler: Arc::new(handler),
            broadcaster,
            stats: Arc::new(ServerStats::default()),
            shutdown: shutdown_tx,
            running: AtomicBool::new(false),
        }
    }

    /// Creates a new server with snapshot support.
    pub fn with_snapshots(
        config: ServerConfig,
        engine: Arc<StateMachineEngine>,
        snapshot_dir: impl AsRef<Path>,
    ) -> Result<Self, ServerError> {
        let (shutdown_tx, _) = broadcast::channel(1);
        let broadcaster = Arc::new(EventBroadcaster::new(DEFAULT_BROADCAST_CAPACITY));
        let mut handler = CommandHandler::with_snapshots(engine, snapshot_dir)?
            .with_broadcaster(broadcaster.clone())
            .with_max_machine_versions(config.max_machine_versions);
        if let Some(ref metrics) = config.metrics {
            handler = handler.with_metrics(metrics.clone());
        }
        Ok(Self {
            config,
            handler: Arc::new(handler),
            broadcaster,
            stats: Arc::new(ServerStats::default()),
            shutdown: shutdown_tx,
            running: AtomicBool::new(false),
        })
    }

    /// Creates a new server with snapshots and authentication.
    pub fn with_snapshots_and_auth(
        config: ServerConfig,
        engine: Arc<StateMachineEngine>,
        snapshot_dir: impl AsRef<Path>,
        auth_config: &AuthConfig,
    ) -> Result<Self, ServerError> {
        let (shutdown_tx, _) = broadcast::channel(1);
        let broadcaster = Arc::new(EventBroadcaster::new(DEFAULT_BROADCAST_CAPACITY));
        let mut handler =
            CommandHandler::with_snapshots_and_auth(engine, snapshot_dir, auth_config)?
                .with_broadcaster(broadcaster.clone())
                .with_max_machine_versions(config.max_machine_versions);
        if let Some(ref metrics) = config.metrics {
            handler = handler.with_metrics(metrics.clone());
        }
        Ok(Self {
            config,
            handler: Arc::new(handler),
            broadcaster,
            stats: Arc::new(ServerStats::default()),
            shutdown: shutdown_tx,
            running: AtomicBool::new(false),
        })
    }

    /// Runs the server.
    pub async fn run(&self) -> Result<(), ServerError> {
        let listener = TcpListener::bind(self.config.bind_addr).await?;
        self.running.store(true, Ordering::SeqCst);

        let tls_mode = if self.config.tls_enabled() {
            "TLS"
        } else {
            "plain"
        };
        tracing::info!(
            "Server listening on {} ({})",
            self.config.bind_addr,
            tls_mode
        );

        let mut shutdown_rx = self.shutdown.subscribe();

        loop {
            tokio::select! {
                result = listener.accept() => {
                    match result {
                        Ok((tcp_stream, addr)) => {
                            if self.stats.connections_active.load(Ordering::Relaxed)
                                >= self.config.max_connections as u64
                            {
                                tracing::warn!("Connection limit reached, rejecting {}", addr);
                                continue;
                            }

                            self.stats.connections_total.fetch_add(1, Ordering::Relaxed);
                            self.stats.connections_active.fetch_add(1, Ordering::Relaxed);

                            // Update metrics if enabled
                            if let Some(ref metrics) = self.config.metrics {
                                metrics.connections_total.inc();
                                metrics.connections_active.inc();
                            }

                            let tls_acceptor = self.config.tls_acceptor.clone();
                            let handler = self.handler.clone();
                            let broadcaster = self.broadcaster.clone();
                            let stats = self.stats.clone();
                            let config = self.config.clone();
                            let mut conn_shutdown = self.shutdown.subscribe();

                            tokio::spawn(async move {
                                // Perform TLS handshake if enabled
                                let stream = match Self::maybe_tls_accept(tcp_stream, tls_acceptor.as_deref(), addr).await {
                                    Ok(s) => s,
                                    Err(e) => {
                                        tracing::warn!("[{}] TLS handshake failed: {}", addr, e);
                                        stats.errors_total.fetch_add(1, Ordering::Relaxed);
                                        stats.connections_active.fetch_sub(1, Ordering::Relaxed);
                                        return;
                                    }
                                };

                                let result = Self::handle_connection(
                                    stream,
                                    addr,
                                    handler,
                                    broadcaster,
                                    config.clone(),
                                    &mut conn_shutdown,
                                )
                                .await;

                                if let Err(e) = result {
                                    tracing::debug!("Connection {} error: {}", addr, e);
                                    stats.errors_total.fetch_add(1, Ordering::Relaxed);
                                }

                                stats.connections_active.fetch_sub(1, Ordering::Relaxed);

                                // Update metrics if enabled
                                if let Some(ref metrics) = config.metrics {
                                    metrics.connections_active.dec();
                                }

                                tracing::info!("Client disconnected: {}", addr);
                            });
                        }
                        Err(e) => {
                            tracing::error!("Accept error: {}", e);
                        }
                    }
                }
                _ = shutdown_rx.recv() => {
                    tracing::info!("Server shutting down");
                    break;
                }
            }
        }

        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Optionally performs TLS handshake on the stream.
    async fn maybe_tls_accept(
        tcp_stream: TcpStream,
        acceptor: Option<&TlsAcceptor>,
        addr: SocketAddr,
    ) -> Result<MaybeTlsStream, ServerError> {
        match acceptor {
            Some(acceptor) => {
                tracing::debug!("[{}] Performing TLS handshake", addr);
                let tls_stream = acceptor
                    .accept(tcp_stream)
                    .await
                    .map_err(|e| ServerError::TlsHandshake(e.to_string()))?;
                tracing::debug!("[{}] TLS handshake complete", addr);
                Ok(MaybeTlsStream::Tls { stream: tls_stream })
            }
            None => Ok(MaybeTlsStream::Plain { stream: tcp_stream }),
        }
    }

    /// Handles a single connection with support for streaming events.
    async fn handle_connection(
        mut stream: MaybeTlsStream,
        addr: SocketAddr,
        handler: Arc<CommandHandler>,
        broadcaster: Arc<EventBroadcaster>,
        config: ServerConfig,
        shutdown: &mut broadcast::Receiver<()>,
    ) -> Result<(), ServerError> {
        let tls_status = if stream.is_tls() { " (TLS)" } else { "" };
        tracing::info!("Client connected: {}{}", addr, tls_status);

        let mut session = Session::new(addr, config.auth_required);
        let mut decoder = Decoder::new();
        let mut buf = [0u8; 8192];

        // Channel for forwarding events from subscription tasks to the main loop
        let (event_tx, mut event_rx) = mpsc::channel::<ForwardedEvent>(256);

        // Track subscription task handles for cleanup
        let mut subscription_tasks: std::collections::HashMap<String, tokio::task::JoinHandle<()>> =
            std::collections::HashMap::new();

        loop {
            tokio::select! {
                biased;

                // Handle incoming event from subscription forwarders
                Some(forwarded) = event_rx.recv() => {
                    let stream_event = StreamEvent {
                        msg_type: "event".to_string(),
                        subscription_id: forwarded.subscription_id.clone(),
                        instance_id: forwarded.event.instance_id,
                        machine: forwarded.event.machine,
                        version: forwarded.event.version,
                        wal_offset: forwarded.event.wal_offset,
                        from_state: forwarded.event.from_state,
                        to_state: forwarded.event.to_state,
                        event: forwarded.event.event,
                        payload: Some(forwarded.event.payload),
                        ctx: if forwarded.include_ctx { Some(forwarded.event.ctx) } else { None },
                    };

                    let event_bytes = match session.wire_mode() {
                        WireMode::BinaryJson => Encoder::encode_json(&stream_event)?,
                        WireMode::Jsonl => {
                            let mut bytes = serde_json::to_vec(&stream_event)?;
                            bytes.push(b'\n');
                            BytesMut::from(&bytes[..])
                        }
                    };

                    // Update events forwarded metric
                    if let Some(ref metrics) = config.metrics {
                        // Determine subscription type from session
                        let sub_type = if session.get_subscription_instance(&forwarded.subscription_id).is_some() {
                            "instance"
                        } else {
                            "all"
                        };
                        metrics.events_forwarded_total.with_label_values(&[sub_type]).inc();
                    }

                    tracing::debug!("[{}] Sending stream event: {} bytes", addr, event_bytes.len());
                    stream.write_all(&event_bytes).await?;
                }

                // Handle incoming data from client
                result = stream.read(&mut buf) => {
                    match result {
                        Ok(0) => {
                            tracing::debug!("[{}] Connection closed by client", addr);
                            Self::cleanup_subscriptions(&session, &broadcaster);
                            Self::abort_subscription_tasks(&mut subscription_tasks);
                            return Ok(());
                        }
                        Ok(n) => {
                            tracing::debug!("[{}] Received {} bytes", addr, n);
                            decoder.extend(&buf[..n]);
                        }
                        Err(e) => {
                            tracing::debug!("[{}] Read error: {}", addr, e);
                            Self::cleanup_subscriptions(&session, &broadcaster);
                            Self::abort_subscription_tasks(&mut subscription_tasks);
                            return Err(ServerError::Io(e));
                        }
                    }
                }

                // Handle idle timeout
                _ = tokio::time::sleep(config.idle_timeout) => {
                    if session.idle_duration() > config.idle_timeout {
                        tracing::debug!("[{}] Idle timeout", addr);
                        Self::cleanup_subscriptions(&session, &broadcaster);
                        Self::abort_subscription_tasks(&mut subscription_tasks);
                        return Ok(());
                    }
                }

                // Handle shutdown signal
                _ = shutdown.recv() => {
                    tracing::debug!("[{}] Shutdown signal received", addr);
                    Self::cleanup_subscriptions(&session, &broadcaster);
                    Self::abort_subscription_tasks(&mut subscription_tasks);
                    return Err(ServerError::ShuttingDown);
                }
            }

            // Process any complete requests
            while let Some(request) = decoder.decode_request()? {
                tracing::info!("[{}] Request: {:?} (id={})", addr, request.op, request.id);

                // Special handling for watch commands to spawn forwarder tasks
                let response = match request.op {
                    Operation::WatchInstance => {
                        match handler.handle_watch_instance(&mut session, &request.params) {
                            Ok((result, receiver)) => {
                                let sub_id =
                                    result["subscription_id"].as_str().unwrap().to_string();
                                let include_ctx =
                                    request.params["include_ctx"].as_bool().unwrap_or(true);

                                // Spawn forwarder task
                                let task = Self::spawn_subscription_forwarder(
                                    sub_id.clone(),
                                    receiver,
                                    None, // No filter for instance subscriptions
                                    include_ctx,
                                    event_tx.clone(),
                                );
                                subscription_tasks.insert(sub_id, task);

                                rstmdb_protocol::Response::ok(&request.id, result)
                            }
                            Err(e) => rstmdb_protocol::Response::error(
                                &request.id,
                                rstmdb_protocol::message::ResponseError::new(
                                    e.error_code(),
                                    e.to_string(),
                                ),
                            ),
                        }
                    }
                    Operation::WatchAll => {
                        match handler.handle_watch_all(&mut session, &request.params) {
                            Ok((result, receiver, filter)) => {
                                let sub_id =
                                    result["subscription_id"].as_str().unwrap().to_string();
                                let include_ctx =
                                    request.params["include_ctx"].as_bool().unwrap_or(true);

                                // Spawn forwarder task with filter
                                let task = Self::spawn_subscription_forwarder(
                                    sub_id.clone(),
                                    receiver,
                                    Some(filter),
                                    include_ctx,
                                    event_tx.clone(),
                                );
                                subscription_tasks.insert(sub_id, task);

                                rstmdb_protocol::Response::ok(&request.id, result)
                            }
                            Err(e) => rstmdb_protocol::Response::error(
                                &request.id,
                                rstmdb_protocol::message::ResponseError::new(
                                    e.error_code(),
                                    e.to_string(),
                                ),
                            ),
                        }
                    }
                    Operation::Unwatch => {
                        let response = handler.handle(&mut session, &request);
                        // Abort the forwarder task for this subscription
                        if let Some(sub_id) = request.params["subscription_id"].as_str() {
                            if let Some(task) = subscription_tasks.remove(sub_id) {
                                task.abort();
                            }
                        }
                        response
                    }
                    _ => handler.handle(&mut session, &request),
                };

                tracing::info!(
                    "[{}] Response: {} (id={})",
                    addr,
                    if response.is_ok() { "OK" } else { "ERROR" },
                    response.id
                );

                // Encode and send response
                let response_bytes = match session.wire_mode() {
                    WireMode::BinaryJson => Encoder::encode_response(&response)?,
                    WireMode::Jsonl => {
                        let mut bytes = serde_json::to_vec(&response)?;
                        bytes.push(b'\n');
                        BytesMut::from(&bytes[..])
                    }
                };

                tracing::debug!("[{}] Writing {} bytes", addr, response_bytes.len());
                stream.write_all(&response_bytes).await?;

                // Check if session is closing
                if session.state() == SessionState::Closing {
                    tracing::debug!("[{}] Session closing", addr);
                    Self::cleanup_subscriptions(&session, &broadcaster);
                    Self::abort_subscription_tasks(&mut subscription_tasks);
                    return Ok(());
                }
            }
        }
    }

    /// Spawns a task that forwards events from a broadcast receiver to an mpsc channel.
    fn spawn_subscription_forwarder(
        subscription_id: String,
        mut receiver: broadcast::Receiver<InstanceEvent>,
        filter: Option<EventFilter>,
        include_ctx: bool,
        tx: mpsc::Sender<ForwardedEvent>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        // Apply filter if present
                        if let Some(ref f) = filter {
                            if !f.matches(&event) {
                                continue;
                            }
                        }

                        let forwarded = ForwardedEvent {
                            subscription_id: subscription_id.clone(),
                            event,
                            include_ctx,
                        };

                        // Send to connection handler; if channel is closed, exit
                        if tx.send(forwarded).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("Subscription {} lagged {} events", subscription_id, n);
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        break;
                    }
                }
            }
        })
    }

    /// Aborts all subscription forwarder tasks.
    fn abort_subscription_tasks(
        tasks: &mut std::collections::HashMap<String, tokio::task::JoinHandle<()>>,
    ) {
        for (_, task) in tasks.drain() {
            task.abort();
        }
    }

    /// Cleans up all subscriptions for a session.
    fn cleanup_subscriptions(session: &Session, broadcaster: &EventBroadcaster) {
        for sub_id in session.subscriptions() {
            broadcaster.unsubscribe(&sub_id);
        }
    }

    /// Initiates server shutdown.
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(());
    }

    /// Returns whether the server is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Returns server statistics.
    pub fn stats(&self) -> &ServerStats {
        &self.stats
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstmdb_wal::{FsyncPolicy, WalConfig};
    use tempfile::TempDir;

    async fn test_server() -> (TempDir, Server) {
        let dir = TempDir::new().unwrap();
        let wal_config = WalConfig::new(dir.path())
            .with_segment_size(4096)
            .with_fsync_policy(FsyncPolicy::EveryWrite);
        let engine = Arc::new(StateMachineEngine::new(wal_config).unwrap());

        // Use a random port
        let config = ServerConfig::new("127.0.0.1:0".parse().unwrap());
        let server = Server::new(config, engine);

        (dir, server)
    }

    #[tokio::test]
    async fn test_server_basic() {
        // This is a basic sanity test - full integration tests would be more comprehensive
        let (_dir, server) = test_server().await;
        assert!(!server.is_running());
    }
}
