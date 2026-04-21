//! Replication lag monitoring.
//!
//! Background task that periodically checks replication lag and logs warnings.

use crate::config::ReplicationConfig;
use crate::metrics::Metrics;
use crate::replication::manager::ReplicationManager;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Runs a background lag monitoring task for the primary.
///
/// Updates per-replica gauges (`replication_replica_lag_entries`,
/// `replication_replica_last_acked_sequence`) and logs a summary of connected
/// replicas and their individual lag.
pub async fn run_primary_lag_monitor(
    manager: Arc<ReplicationManager>,
    config: ReplicationConfig,
    metrics: Option<Arc<Metrics>>,
    mut shutdown: broadcast::Receiver<()>,
) {
    let check_interval = config.lag_check_interval();

    loop {
        tokio::select! {
            _ = tokio::time::sleep(check_interval) => {
                let stats = manager.replica_stats();
                let connected = stats.len();

                if let Some(ref m) = metrics {
                    m.replication_connected_replicas.set(connected as f64);
                    // Update per-replica gauges
                    for (replica_id, acked, lag) in &stats {
                        m.replication_replica_lag_entries
                            .with_label_values(&[replica_id.as_str()])
                            .set(*lag as f64);
                        m.replication_replica_last_acked_sequence
                            .with_label_values(&[replica_id.as_str()])
                            .set(*acked as f64);
                    }
                }

                if connected == 0 {
                    tracing::debug!("No replicas connected");
                    continue;
                }

                for (replica_id, acked, lag) in &stats {
                    if *lag > config.max_lag_entries {
                        tracing::warn!(
                            "Replica {} lag: {} entries (acked={}, threshold={})",
                            replica_id, lag, acked, config.max_lag_entries
                        );
                    } else {
                        tracing::debug!(
                            "Replica {}: acked={}, lag={}",
                            replica_id, acked, lag
                        );
                    }
                }

                tracing::debug!(
                    "Replication status: {} replica(s) connected",
                    connected,
                );
            }
            _ = shutdown.recv() => {
                tracing::info!("Lag monitor shutting down");
                return;
            }
        }
    }
}

/// Runs a background lag monitoring task for a replica.
///
/// Updates `replication_lag_entries` (entries behind primary) and
/// `replication_lag_seconds` (time behind primary). Logs warnings when
/// either exceeds the configured thresholds.
pub async fn run_replica_lag_monitor(
    replica_client: &crate::replication::replica_client::ReplicaClient,
    config: ReplicationConfig,
    metrics: Option<Arc<Metrics>>,
    mut shutdown: broadcast::Receiver<()>,
) {
    let check_interval = config.lag_check_interval();

    loop {
        tokio::select! {
            _ = tokio::time::sleep(check_interval) => {
                let lag_entries = replica_client.lag_entries();
                let lag_seconds = replica_client.lag_seconds();

                if let Some(ref m) = metrics {
                    m.replication_lag_entries.set(lag_entries as f64);
                    m.replication_lag_seconds.set(lag_seconds);
                }

                let over_entries = lag_entries > config.max_lag_entries;
                let over_seconds = lag_seconds > config.max_lag_seconds as f64;

                if over_entries || over_seconds {
                    tracing::warn!(
                        "Replication lag: {} entries ({:.2}s) — thresholds: {} entries / {}s",
                        lag_entries, lag_seconds,
                        config.max_lag_entries, config.max_lag_seconds,
                    );
                } else if lag_entries > 0 {
                    tracing::debug!(
                        "Replication lag: {} entries ({:.2}s)",
                        lag_entries, lag_seconds,
                    );
                }
            }
            _ = shutdown.recv() => {
                tracing::info!("Replica lag monitor shutting down");
                return;
            }
        }
    }
}
