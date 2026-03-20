//! Replication lag monitoring.
//!
//! Background task that periodically checks replication lag and logs warnings.

use crate::config::ReplicationConfig;
use crate::replication::manager::ReplicationManager;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Runs a background lag monitoring task for the primary.
pub async fn run_primary_lag_monitor(
    manager: Arc<ReplicationManager>,
    config: ReplicationConfig,
    mut shutdown: broadcast::Receiver<()>,
) {
    let check_interval = config.lag_check_interval();

    loop {
        tokio::select! {
            _ = tokio::time::sleep(check_interval) => {
                let connected = manager.connected_replica_count();
                if connected == 0 {
                    tracing::debug!("No replicas connected");
                    continue;
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
pub async fn run_replica_lag_monitor(
    replica_client: &crate::replication::replica_client::ReplicaClient,
    config: ReplicationConfig,
    mut shutdown: broadcast::Receiver<()>,
) {
    let check_interval = config.lag_check_interval();

    loop {
        tokio::select! {
            _ = tokio::time::sleep(check_interval) => {
                let lag = replica_client.lag_entries();

                if lag > config.max_lag_entries {
                    tracing::warn!(
                        "Replication lag is {} entries (threshold: {})",
                        lag,
                        config.max_lag_entries,
                    );
                } else if lag > 0 {
                    tracing::debug!(
                        "Replication lag: {} entries",
                        lag,
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
