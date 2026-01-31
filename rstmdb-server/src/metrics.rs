//! Prometheus metrics for rstmdb server.
//!
//! This module provides:
//! - Metrics registry with counters, gauges, and histograms
//! - HTTP server to expose metrics at `/metrics` endpoint

use http_body_util::Full;
use hyper::body::Bytes;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use prometheus::{
    Counter, CounterVec, Encoder, Gauge, GaugeVec, HistogramOpts, HistogramVec, Opts, Registry,
    TextEncoder,
};
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
}

impl Metrics {
    /// Creates a new Metrics instance with all metrics registered.
    pub fn new() -> Result<Self, prometheus::Error> {
        let registry = Registry::new();

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

        // WAL
        let wal_entries = Gauge::with_opts(Opts::new(
            "rstmdb_wal_entries",
            "Number of entries in the WAL",
        ))?;
        registry.register(Box::new(wal_entries.clone()))?;

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
        })
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
        assert!(metrics.encode().len() > 0);
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
