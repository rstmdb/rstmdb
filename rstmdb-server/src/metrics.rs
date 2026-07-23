//! Prometheus metrics for rstmdb server.
//!
//! This module provides:
//! - Metrics registry with counters, gauges, and histograms
//! - Process metrics (CPU, memory, file descriptors)
//! - HTTP server to expose metrics at `/metrics` endpoint

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use parking_lot::Mutex;
#[cfg(target_os = "linux")]
use prometheus::process_collector::ProcessCollector;
use prometheus::{
    Counter, CounterVec, Encoder, Gauge, GaugeVec, HistogramOpts, HistogramVec, Opts, Registry,
    TextEncoder,
};
use rstmdb_wal::WalStats;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::broadcast;

/// Request duration histogram buckets (in seconds).
const DURATION_BUCKETS: &[f64] = &[0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0];

/// Prometheus metrics for the rstmdb server.
#[derive(Clone)]
pub struct Metrics {
    registry: Registry,
    /// Total connections accepted.
    pub connections_total: Counter,
    /// Currently active connections.
    pub connections_active: Gauge,
    /// Total requests by operation type.
    pub requests_total: CounterVec,
    /// Total errors by error code.
    pub errors_total: CounterVec,
    /// Request duration histogram by operation.
    pub request_duration: HistogramVec,
    /// Active watch subscriptions by type.
    pub subscriptions_active: GaugeVec,
    /// Events forwarded by type.
    pub events_forwarded_total: CounterVec,
    /// Total state machine instances.
    pub instances_total: Gauge,
    /// Total registered machines.
    pub machines_total: Gauge,
    /// WAL entry count.
    pub wal_entries: Gauge,
    /// WAL segment count.
    pub wal_segments: Gauge,
    /// WAL total size in bytes.
    pub wal_size_bytes: Gauge,
    /// Total bytes written to WAL.
    pub wal_bytes_written_total: Counter,
    /// Total bytes read from WAL.
    pub wal_bytes_read_total: Counter,
    /// Total WAL write operations.
    pub wal_writes_total: Counter,
    /// Total WAL read operations.
    pub wal_reads_total: Counter,
    /// Total WAL fsync operations.
    pub wal_fsyncs_total: Counter,
    /// Last reported WAL stats (for computing counter deltas).
    last_wal_stats: Arc<Mutex<WalStats>>,
    /// Replication lag in entries (per replica for primary, single value for replica).
    pub replication_lag_entries: Gauge,
    /// Replication lag in seconds.
    pub replication_lag_seconds: Gauge,
    /// Number of connected replicas (primary only).
    pub replication_connected_replicas: Gauge,
    /// Total replication entries sent (primary only).
    pub replication_entries_sent_total: Counter,
    /// Total sync replication timeouts (primary only).
    pub replication_sync_timeouts_total: Counter,
    /// Total times a replica was disconnected because its send channel filled
    /// up (slow replica). The replica will catch up from WAL on reconnect.
    pub replication_slow_replica_disconnects_total: Counter,
    /// Per-replica lag in entries (primary only), labeled by replica_id.
    pub replication_replica_lag_entries: GaugeVec,
    /// Per-replica last-acked sequence (primary only), labeled by replica_id.
    pub replication_replica_last_acked_sequence: GaugeVec,
}

impl Metrics {
    /// Creates a new Metrics instance with all metrics registered.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

        // Register process collector for CPU, memory, and file descriptor metrics (Linux only)
        #[cfg(target_os = "linux")]
        {
            let process_collector = ProcessCollector::for_self();
            registry.register(Box::new(process_collector))?;
        }

        // Connections
        let connections_total = Counter::with_opts(Opts::new(
            "rstmdb_connections_total",
            "Total number of connections accepted",
        ))?;
        registry.register(Box::new(connections_total.clone()))?;

        let connections_active = Gauge::with_opts(Opts::new(
            "rstmdb_connections_active",
            "Number of currently active connections",
        ))?;
        registry.register(Box::new(connections_active.clone()))?;

        // Requests
        let requests_total = CounterVec::new(
            Opts::new("rstmdb_requests_total", "Total requests by operation"),
            &["operation"],
        )?;
        registry.register(Box::new(requests_total.clone()))?;

        // Errors
        let errors_total = CounterVec::new(
            Opts::new("rstmdb_errors_total", "Total errors by error code"),
            &["code"],
        )?;
        registry.register(Box::new(errors_total.clone()))?;

        // Request duration
        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "rstmdb_request_duration_seconds",
                "Request duration in seconds by operation",
            )
            .buckets(DURATION_BUCKETS.to_vec()),
            &["operation"],
        )?;
        registry.register(Box::new(request_duration.clone()))?;

        // Subscriptions
        let subscriptions_active = GaugeVec::new(
            Opts::new(
                "rstmdb_subscriptions_active",
                "Active watch subscriptions by type",
            ),
            &["type"],
        )?;
        registry.register(Box::new(subscriptions_active.clone()))?;

        // Events forwarded
        let events_forwarded_total = CounterVec::new(
            Opts::new(
                "rstmdb_events_forwarded_total",
                "Total events forwarded to subscribers by type",
            ),
            &["type"],
        )?;
        registry.register(Box::new(events_forwarded_total.clone()))?;

        // Instances
        let instances_total = Gauge::with_opts(Opts::new(
            "rstmdb_instances_total",
            "Total number of state machine instances",
        ))?;
        registry.register(Box::new(instances_total.clone()))?;

        // Machines
        let machines_total = Gauge::with_opts(Opts::new(
            "rstmdb_machines_total",
            "Total number of registered state machines",
        ))?;
        registry.register(Box::new(machines_total.clone()))?;

        // WAL metrics
        let wal_entries = Gauge::with_opts(Opts::new(
            "rstmdb_wal_entries",
            "Number of entries in the WAL",
        ))?;
        registry.register(Box::new(wal_entries.clone()))?;

        let wal_segments =
            Gauge::with_opts(Opts::new("rstmdb_wal_segments", "Number of WAL segments"))?;
        registry.register(Box::new(wal_segments.clone()))?;

        let wal_size_bytes = Gauge::with_opts(Opts::new(
            "rstmdb_wal_size_bytes",
            "Total size of WAL on disk in bytes",
        ))?;
        registry.register(Box::new(wal_size_bytes.clone()))?;

        let wal_bytes_written_total = Counter::with_opts(Opts::new(
            "rstmdb_wal_bytes_written_total",
            "Total bytes written to WAL",
        ))?;
        registry.register(Box::new(wal_bytes_written_total.clone()))?;

        let wal_bytes_read_total = Counter::with_opts(Opts::new(
            "rstmdb_wal_bytes_read_total",
            "Total bytes read from WAL",
        ))?;
        registry.register(Box::new(wal_bytes_read_total.clone()))?;

        let wal_writes_total = Counter::with_opts(Opts::new(
            "rstmdb_wal_writes_total",
            "Total WAL write operations",
        ))?;
        registry.register(Box::new(wal_writes_total.clone()))?;

        let wal_reads_total = Counter::with_opts(Opts::new(
            "rstmdb_wal_reads_total",
            "Total WAL read operations",
        ))?;
        registry.register(Box::new(wal_reads_total.clone()))?;

        let wal_fsyncs_total = Counter::with_opts(Opts::new(
            "rstmdb_wal_fsyncs_total",
            "Total WAL fsync operations",
        ))?;
        registry.register(Box::new(wal_fsyncs_total.clone()))?;

        // Replication metrics
        let replication_lag_entries = Gauge::with_opts(Opts::new(
            "rstmdb_replication_lag_entries",
            "Replication lag in entries",
        ))?;
        registry.register(Box::new(replication_lag_entries.clone()))?;

        let replication_lag_seconds = Gauge::with_opts(Opts::new(
            "rstmdb_replication_lag_seconds",
            "Replication lag in seconds",
        ))?;
        registry.register(Box::new(replication_lag_seconds.clone()))?;

        let replication_connected_replicas = Gauge::with_opts(Opts::new(
            "rstmdb_replication_connected_replicas",
            "Number of connected replicas",
        ))?;
        registry.register(Box::new(replication_connected_replicas.clone()))?;

        let replication_entries_sent_total = Counter::with_opts(Opts::new(
            "rstmdb_replication_entries_sent_total",
            "Total replication entries sent to replicas",
        ))?;
        registry.register(Box::new(replication_entries_sent_total.clone()))?;

        let replication_sync_timeouts_total = Counter::with_opts(Opts::new(
            "rstmdb_replication_sync_timeouts_total",
            "Total sync replication timeouts",
        ))?;
        registry.register(Box::new(replication_sync_timeouts_total.clone()))?;

        let replication_slow_replica_disconnects_total = Counter::with_opts(Opts::new(
            "rstmdb_replication_slow_replica_disconnects_total",
            "Total times a replica was disconnected due to a full send channel (slow replica)",
        ))?;
        registry.register(Box::new(replication_slow_replica_disconnects_total.clone()))?;

        let replication_replica_lag_entries = GaugeVec::new(
            Opts::new(
                "rstmdb_replication_replica_lag_entries",
                "Per-replica lag in entries (primary side), labeled by replica_id",
            ),
            &["replica_id"],
        )?;
        registry.register(Box::new(replication_replica_lag_entries.clone()))?;

        let replication_replica_last_acked_sequence = GaugeVec::new(
            Opts::new(
                "rstmdb_replication_replica_last_acked_sequence",
                "Per-replica last-acked WAL sequence (primary side), labeled by replica_id",
            ),
            &["replica_id"],
        )?;
        registry.register(Box::new(replication_replica_last_acked_sequence.clone()))?;

        Ok(Self {
            registry,
            connections_total,
            connections_active,
            requests_total,
            errors_total,
            request_duration,
            subscriptions_active,
            events_forwarded_total,
            instances_total,
            machines_total,
            wal_entries,
            wal_segments,
            wal_size_bytes,
            wal_bytes_written_total,
            wal_bytes_read_total,
            wal_writes_total,
            wal_reads_total,
            wal_fsyncs_total,
            last_wal_stats: Arc::new(Mutex::new(WalStats::default())),
            replication_lag_entries,
            replication_lag_seconds,
            replication_connected_replicas,
            replication_entries_sent_total,
            replication_sync_timeouts_total,
            replication_slow_replica_disconnects_total,
            replication_replica_lag_entries,
            replication_replica_last_acked_sequence,
        })
    }

    /// Updates WAL I/O counters from the given stats.
    ///
    /// This computes the delta from the last reported stats and increments
    /// the counters accordingly.
    pub fn update_wal_stats(&self, stats: WalStats) {
        let mut last = self.last_wal_stats.lock();

        // Compute deltas (handle potential counter reset)
        let bytes_written_delta = stats.bytes_written.saturating_sub(last.bytes_written);
        let bytes_read_delta = stats.bytes_read.saturating_sub(last.bytes_read);
        let writes_delta = stats.writes.saturating_sub(last.writes);
        let reads_delta = stats.reads.saturating_sub(last.reads);
        let fsyncs_delta = stats.fsyncs.saturating_sub(last.fsyncs);

        // Update counters
        if bytes_written_delta > 0 {
            self.wal_bytes_written_total
                .inc_by(bytes_written_delta as f64);
        }
        if bytes_read_delta > 0 {
            self.wal_bytes_read_total.inc_by(bytes_read_delta as f64);
        }
        if writes_delta > 0 {
            self.wal_writes_total.inc_by(writes_delta as f64);
        }
        if reads_delta > 0 {
            self.wal_reads_total.inc_by(reads_delta as f64);
        }
        if fsyncs_delta > 0 {
            self.wal_fsyncs_total.inc_by(fsyncs_delta as f64);
        }

        // Update last known stats
        *last = stats;
    }

    /// Encodes all metrics in Prometheus text format.
    pub fn encode(&self) -> Vec<u8> {
        let mut buffer = Vec::new();
        let encoder = TextEncoder::new();
        let metric_families = self.registry.gather();
        encoder.encode(&metric_families, &mut buffer).unwrap();
        buffer
    }

    /// Returns a reference to the registry.
    pub fn registry(&self) -> &Registry {
        &self.registry
    }
}

/// Refreshes engine-derived gauge metrics (instance count, machine count,
/// WAL entries/segments/size, WAL I/O counters) from the current engine state.
///
/// This is safe to call from any thread. The primary also updates these
/// from write handlers, but replicas only receive data via
/// `apply_replicated_entry` — which doesn't update gauges — so a periodic
/// refresher is needed to keep replica-side metrics accurate.
pub fn refresh_engine_gauges(engine: &rstmdb_core::StateMachineEngine, metrics: &Metrics) {
    let instances = engine.get_all_instances();
    metrics.instances_total.set(instances.len() as f64);

    let machines = engine.list_machines();
    let machine_count: usize = machines.values().map(|v| v.len()).sum();
    metrics.machines_total.set(machine_count as f64);

    let wal = engine.wal();
    // next_sequence is 1-based; subtract 1 to get actual entry count
    let entry_count = wal.next_sequence().saturating_sub(1);
    metrics.wal_entries.set(entry_count as f64);
    metrics.wal_segments.set(wal.segment_ids().len() as f64);
    metrics.wal_size_bytes.set(wal.total_size() as f64);

    metrics.update_wal_stats(wal.stats());
}

/// Runs a periodic gauge refresher task. Call this from main.rs when metrics
/// are enabled — it ensures WAL/instance/machine gauges reflect current state
/// even on read-only replicas (which don't trigger write-path gauge updates).
pub async fn run_gauge_refresher(
    engine: std::sync::Arc<rstmdb_core::StateMachineEngine>,
    metrics: std::sync::Arc<Metrics>,
    interval: std::time::Duration,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    let mut ticker = tokio::time::interval(interval);
    loop {
        tokio::select! {
            _ = ticker.tick() => {
                refresh_engine_gauges(&engine, &metrics);
            }
            _ = shutdown.recv() => {
                tracing::info!("Gauge refresher shutting down");
                return;
            }
        }
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new().expect("Failed to create default metrics")
    }
}

/// Runs the HTTP metrics server.
///
/// The server listens on the given address and serves metrics at `/metrics`.
pub async fn run_metrics_server(
    addr: SocketAddr,
    metrics: Arc<Metrics>,
    mut shutdown: broadcast::Receiver<()>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("Metrics server listening on http://{}/metrics", addr);

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let metrics = metrics.clone();
                        tokio::spawn(async move {
                            let io = TokioIo::new(stream);
                            let service = service_fn(move |req| {
                                let metrics = metrics.clone();
                                async move { handle_request(req, metrics).await }
                            });
                            if let Err(e) = http1::Builder::new()
                                .serve_connection(io, service)
                                .await
                            {
                                tracing::debug!("Metrics connection error: {}", e);
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Metrics server accept error: {}", e);
                    }
                }
            }
            _ = shutdown.recv() => {
                tracing::info!("Metrics server shutting down");
                break;
            }
        }
    }

    Ok(())
}

/// Handles an HTTP request to the metrics server.
async fn handle_request(
    req: Request<hyper::body::Incoming>,
    metrics: Arc<Metrics>,
) -> Result<Response<Full<Bytes>>, hyper::Error> {
    let response = match req.uri().path() {
        "/metrics" => {
            let body = metrics.encode();
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/plain; version=0.0.4; charset=utf-8")
                .body(Full::new(Bytes::from(body)))
                .unwrap()
        }
        "/health" | "/healthz" => Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::from("OK")))
            .unwrap(),
        "/" => Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "text/html")
            .body(Full::new(Bytes::from(
                r#"<!DOCTYPE html>
<html>
<head><title>rstmdb Metrics</title></head>
<body>
<h1>rstmdb Metrics</h1>
<p><a href="/metrics">Metrics</a></p>
</body>
</html>"#,
            )))
            .unwrap(),
        _ => Response::builder()
            .status(StatusCode::NOT_FOUND)
            .body(Full::new(Bytes::from("Not Found")))
            .unwrap(),
    };

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_creation() {
        let metrics = Metrics::new().unwrap();

        // Test incrementing counters
        metrics.connections_total.inc();
        metrics.connections_active.inc();
        metrics.requests_total.with_label_values(&["PING"]).inc();
        metrics.errors_total.with_label_values(&["NOT_FOUND"]).inc();

        // Test histogram
        metrics
            .request_duration
            .with_label_values(&["PING"])
            .observe(0.001);

        // Test encoding
        let encoded = metrics.encode();
        let encoded_str = String::from_utf8(encoded).unwrap();

        assert!(encoded_str.contains("rstmdb_connections_total"));
        assert!(encoded_str.contains("rstmdb_connections_active"));
        assert!(encoded_str.contains("rstmdb_requests_total"));
        assert!(encoded_str.contains("rstmdb_errors_total"));
        assert!(encoded_str.contains("rstmdb_request_duration_seconds"));
    }

    #[test]
    fn test_metrics_default() {
        let metrics = Metrics::default();
        assert!(!metrics.encode().is_empty());
    }

    #[test]
    fn test_all_metrics_registered() {
        let metrics = Metrics::new().unwrap();

        // Verify all gauges work
        metrics.connections_active.set(5.0);
        metrics.instances_total.set(100.0);
        metrics.machines_total.set(10.0);
        metrics.wal_entries.set(50000.0);
        metrics
            .subscriptions_active
            .with_label_values(&["instance"])
            .set(3.0);
        metrics
            .subscriptions_active
            .with_label_values(&["all"])
            .set(2.0);

        // Verify counters work
        metrics
            .events_forwarded_total
            .with_label_values(&["instance"])
            .inc();
        metrics
            .events_forwarded_total
            .with_label_values(&["all"])
            .inc();

        let encoded = String::from_utf8(metrics.encode()).unwrap();
        assert!(encoded.contains("rstmdb_instances_total 100"));
        assert!(encoded.contains("rstmdb_machines_total 10"));
        assert!(encoded.contains("rstmdb_wal_entries 50000"));
    }
}
