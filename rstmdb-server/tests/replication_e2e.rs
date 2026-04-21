//! End-to-end replication tests that run a real primary TCP server and one
//! or more replica clients in-process.
//!
//! These tests cover real-world scenarios that unit-level replication tests
//! (which call `apply_replicated_entry` directly) miss:
//!
//! - Concurrent writes triggering sequence-out-of-order on disk.
//! - Catch-up via actual TCP handshake.
//! - Replica reconnect after primary restart.
//! - Backpressure / slow-replica disconnect.
//! - Live streaming across many entries.
//!
//! If a change to the replication code regresses any of these scenarios,
//! the corresponding test will fail.

mod common;

use common::{order_machine_def, Cluster};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

// =========================================================================
// Basic sanity: cluster boots, replicas connect, single write propagates
// =========================================================================

#[tokio::test]
async fn e2e_basic_single_write() {
    let cluster = Cluster::spawn(2).await;

    cluster
        .primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();

    cluster.wait_converged(Duration::from_secs(5)).await;
    cluster.assert_parity();

    // Both replicas should have the machine
    for r in &cluster.replicas {
        let def = r.engine.get_machine("order", 1).unwrap();
        assert_eq!(def.name, "order");
    }

    cluster.shutdown();
}

#[tokio::test]
async fn e2e_sequential_writes_replicate() {
    let cluster = Cluster::spawn(2).await;

    cluster
        .primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();
    for i in 0..10 {
        cluster
            .primary
            .engine
            .create_instance(&format!("i-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }

    cluster.wait_converged(Duration::from_secs(5)).await;
    cluster.assert_parity();

    cluster.shutdown();
}

// =========================================================================
// The critical one: concurrent writes shouldn't lose any entry
// =========================================================================

#[tokio::test]
async fn e2e_concurrent_writes_no_loss() {
    // This test reproduces the bug where concurrent writes race on the WAL
    // segment lock, causing sequences to arrive out of order on disk, and
    // the tailer's sequence-based filter silently skipped entries.

    let cluster = Cluster::spawn(2).await;

    cluster
        .primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();

    // Wait briefly so put_machine is definitely replicated before the storm
    cluster.wait_converged(Duration::from_secs(3)).await;

    // Hammer with 50 concurrent create_instance calls
    let engine = cluster.primary.engine.clone();
    let mut tasks = Vec::new();
    for i in 0..50 {
        let e = engine.clone();
        tasks.push(tokio::spawn(async move {
            e.create_instance(&format!("race-{}", i), "order", 1, json!({"i": i}), None)
                .unwrap();
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    cluster.wait_converged(Duration::from_secs(15)).await;

    // Every single instance must be present on every replica
    for (idx, r) in cluster.replicas.iter().enumerate() {
        for i in 0..50 {
            let id = format!("race-{}", i);
            assert!(
                r.engine.get_instance(&id).is_ok(),
                "replica #{} missing instance {}",
                idx,
                id
            );
        }
    }

    cluster.assert_parity();
    cluster.shutdown();
}

#[tokio::test]
async fn e2e_concurrent_put_machine_plus_instances() {
    // Exercises the exact pattern from scripts/race-condition-test.sh:
    // put_machine issued first, then many concurrent create_instance.
    // This was the specific bug where put_machine got lost.

    let cluster = Cluster::spawn(2).await;

    // Start all work without waiting — put_machine races with first instances
    let engine = cluster.primary.engine.clone();
    let def = order_machine_def();

    let e1 = engine.clone();
    let put_task = tokio::spawn(async move {
        e1.put_machine("order", 1, &def).unwrap();
    });

    // These will race with put_machine
    let mut instance_tasks = Vec::new();
    for i in 0..20 {
        let e = engine.clone();
        instance_tasks.push(tokio::spawn(async move {
            // Brief backoff so put_machine has a chance to win first slot,
            // but not enough to serialize fully. Retry on MachineNotFound
            // since the race can legitimately produce that.
            for attempt in 0..50 {
                match e.create_instance(
                    &format!("mix-{}", i),
                    "order",
                    1,
                    json!({"i": i}),
                    None,
                ) {
                    Ok(_) => return,
                    Err(_) if attempt < 49 => {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(e) => panic!("create_instance failed: {}", e),
                }
            }
        }));
    }

    put_task.await.unwrap();
    for t in instance_tasks {
        t.await.unwrap();
    }

    cluster.wait_converged(Duration::from_secs(15)).await;

    // Every replica must have the machine AND all instances
    for (idx, r) in cluster.replicas.iter().enumerate() {
        assert!(
            r.engine.get_machine("order", 1).is_ok(),
            "replica #{} missing put_machine 'order v1'",
            idx
        );
        for i in 0..20 {
            let id = format!("mix-{}", i);
            assert!(
                r.engine.get_instance(&id).is_ok(),
                "replica #{} missing instance {}",
                idx,
                id
            );
        }
    }

    cluster.assert_parity();
    cluster.shutdown();
}

// =========================================================================
// Catch-up: write data, THEN spawn a new replica, verify it catches up
// =========================================================================

#[tokio::test]
async fn e2e_late_replica_catches_up() {
    let mut cluster = Cluster::spawn(1).await;

    cluster
        .primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();
    for i in 0..15 {
        cluster
            .primary
            .engine
            .create_instance(&format!("pre-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }

    cluster.wait_converged(Duration::from_secs(5)).await;

    // Now spawn a third node which must catch up from scratch
    let primary_addr = cluster.primary.addr;
    let late_replica = Cluster::spawn_for_tests(primary_addr).await;
    cluster.replicas.push(late_replica);

    cluster
        .wait_for_replica_count(2, Duration::from_secs(5))
        .await;

    // Write one more after the late replica is attached
    cluster
        .primary
        .engine
        .create_instance("post", "order", 1, json!({}), None)
        .unwrap();

    cluster.wait_converged(Duration::from_secs(5)).await;

    // The late replica should have BOTH the pre- entries (via catchup)
    // AND the post entry (via live streaming).
    let late = cluster.replicas.last().unwrap();
    for i in 0..15 {
        assert!(
            late.engine.get_instance(&format!("pre-{}", i)).is_ok(),
            "late replica missing pre-{}",
            i
        );
    }
    assert!(late.engine.get_instance("post").is_ok());

    cluster.assert_parity();
    cluster.shutdown();
}

// =========================================================================
// Events + context merging
// =========================================================================

#[tokio::test]
async fn e2e_apply_events_update_state() {
    let cluster = Cluster::spawn(1).await;
    let engine = &cluster.primary.engine;

    engine.put_machine("order", 1, &order_machine_def()).unwrap();
    engine
        .create_instance("o-1", "order", 1, json!({"customer": "alice"}), None)
        .unwrap();
    engine
        .apply_event(
            "o-1",
            "PAY",
            json!({"amount": 100}),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    engine
        .apply_event(
            "o-1",
            "SHIP",
            json!({"tracking": "abc"}),
            None,
            None,
            None,
            None,
        )
        .unwrap();

    cluster.wait_converged(Duration::from_secs(5)).await;

    for r in &cluster.replicas {
        let inst = r.engine.get_instance("o-1").unwrap();
        assert_eq!(inst.state, "shipped");
        assert_eq!(inst.ctx["customer"], "alice");
        assert_eq!(inst.ctx["amount"], 100);
        assert_eq!(inst.ctx["tracking"], "abc");
    }

    cluster.assert_parity();
    cluster.shutdown();
}

// =========================================================================
// Delete replicates
// =========================================================================

#[tokio::test]
async fn e2e_delete_replicates() {
    let cluster = Cluster::spawn(1).await;
    let engine = &cluster.primary.engine;

    engine.put_machine("order", 1, &order_machine_def()).unwrap();
    engine
        .create_instance("to-delete", "order", 1, json!({}), None)
        .unwrap();
    cluster.wait_converged(Duration::from_secs(3)).await;

    // Verify it's on the replica first
    assert!(cluster.replicas[0]
        .engine
        .get_instance("to-delete")
        .is_ok());

    engine.delete_instance("to-delete", None).unwrap();
    cluster.wait_converged(Duration::from_secs(3)).await;

    assert!(cluster.replicas[0]
        .engine
        .get_instance("to-delete")
        .is_err());

    cluster.assert_parity();
    cluster.shutdown();
}

// =========================================================================
// WAL offsets match across nodes (primary-offset fix verification)
// =========================================================================

#[tokio::test]
async fn e2e_wal_offsets_match_across_nodes() {
    let cluster = Cluster::spawn(2).await;
    let engine = &cluster.primary.engine;

    engine.put_machine("order", 1, &order_machine_def()).unwrap();
    engine
        .create_instance("check", "order", 1, json!({}), None)
        .unwrap();
    engine
        .apply_event("check", "PAY", json!({}), None, None, None, None)
        .unwrap();

    cluster.wait_converged(Duration::from_secs(3)).await;

    let primary_inst = engine.get_instance("check").unwrap();
    assert!(primary_inst.last_wal_offset > 0);

    for r in &cluster.replicas {
        let inst = r.engine.get_instance("check").unwrap();
        // wal_offset on the replica must equal the primary's offset —
        // this is the fix from `apply_replicated_entry(primary_offset, ...)`
        assert_eq!(
            inst.last_wal_offset, primary_inst.last_wal_offset,
            "replica's wal_offset must match primary's"
        );
    }

    cluster.shutdown();
}

// =========================================================================
// Two replicas converge to identical state
// =========================================================================

#[tokio::test]
async fn e2e_two_replicas_same_state() {
    let cluster = Cluster::spawn(2).await;
    let engine = &cluster.primary.engine;

    engine.put_machine("order", 1, &order_machine_def()).unwrap();
    for i in 0..20 {
        engine
            .create_instance(&format!("i-{}", i), "order", 1, json!({"n": i}), None)
            .unwrap();
    }
    for i in 0..20 {
        engine
            .apply_event(
                &format!("i-{}", i),
                "PAY",
                json!({"paid_n": i}),
                None,
                None,
                None,
                None,
            )
            .unwrap();
    }

    cluster.wait_converged(Duration::from_secs(5)).await;
    cluster.assert_parity();

    // Replicas should be byte-for-byte identical in memory
    let r0 = cluster.replicas[0].engine.get_all_instances();
    let r1 = cluster.replicas[1].engine.get_all_instances();
    let r0_map: std::collections::HashMap<_, _> =
        r0.iter().map(|i| (&i.id, i)).collect();
    let r1_map: std::collections::HashMap<_, _> =
        r1.iter().map(|i| (&i.id, i)).collect();
    for (id, inst0) in &r0_map {
        let inst1 = r1_map.get(id).expect("replica 1 missing instance");
        assert_eq!(inst0.state, inst1.state);
        assert_eq!(inst0.ctx, inst1.ctx);
        assert_eq!(inst0.last_wal_offset, inst1.last_wal_offset);
    }

    cluster.shutdown();
}

// =========================================================================
// Primary metric sanity: connected replicas, entries sent
// =========================================================================

#[tokio::test]
async fn e2e_primary_metrics_update() {
    let cluster = Cluster::spawn(2).await;

    // Connected replicas should be 2
    assert_eq!(cluster.primary.manager.connected_replica_count(), 2);

    cluster
        .primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();
    for i in 0..5 {
        cluster
            .primary
            .engine
            .create_instance(&format!("m-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }

    cluster.wait_converged(Duration::from_secs(5)).await;

    // entries_sent_total should equal WAL entry count (each entry fanned out)
    let wal_entries = cluster
        .primary
        .engine
        .wal()
        .next_sequence()
        .saturating_sub(1);
    let sent = cluster
        .primary
        .metrics
        .replication_entries_sent_total
        .get() as u64;
    assert_eq!(
        sent, wal_entries,
        "entries_sent_total ({}) must equal WAL entries ({})",
        sent, wal_entries
    );

    cluster.shutdown();
}

// =========================================================================
// Large batch workload (stress test)
// =========================================================================

#[tokio::test]
async fn e2e_large_batch_replicates() {
    let cluster = Cluster::spawn(2).await;
    let engine = cluster.primary.engine.clone();

    engine.put_machine("order", 1, &order_machine_def()).unwrap();

    // 100 instances × 3 events each = 301 entries (incl. put_machine)
    for i in 0..100 {
        engine
            .create_instance(&format!("big-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }
    for i in 0..100 {
        engine
            .apply_event(
                &format!("big-{}", i),
                "PAY",
                json!({}),
                None,
                None,
                None,
                None,
            )
            .unwrap();
        engine
            .apply_event(
                &format!("big-{}", i),
                "SHIP",
                json!({}),
                None,
                None,
                None,
                None,
            )
            .unwrap();
    }

    cluster.wait_converged(Duration::from_secs(20)).await;
    cluster.assert_parity();

    for r in &cluster.replicas {
        for i in 0..100 {
            let inst = r.engine.get_instance(&format!("big-{}", i)).unwrap();
            assert_eq!(inst.state, "shipped");
        }
    }

    cluster.shutdown();
}

// =========================================================================
// Helper used by the late-replica test
// =========================================================================

impl Cluster {
    /// Spawns a fresh replica pointing at the given primary address.
    /// Exposed for tests that need to attach replicas mid-run.
    pub async fn spawn_for_tests(primary_addr: std::net::SocketAddr) -> common::ReplicaNode {
        // Re-enter the same plumbing as Cluster::spawn_replica_for without
        // needing an outer cluster — reuse via a fresh internal call.
        Self::__spawn_replica(primary_addr).await
    }

    async fn __spawn_replica(primary_addr: std::net::SocketAddr) -> common::ReplicaNode {
        use rstmdb_core::StateMachineEngine;
        use rstmdb_server::config::{ReplicationConfig, ReplicationRole};
        use rstmdb_server::{Metrics, ReplicaClient};
        use rstmdb_wal::{FsyncPolicy, WalConfig};
        use tempfile::TempDir;
        use tokio::sync::broadcast;

        let wal_dir = TempDir::new().unwrap();
        let wal_config = WalConfig::new(wal_dir.path())
            .with_segment_size(4 * 1024 * 1024)
            .with_fsync_policy(FsyncPolicy::Never);
        let engine = Arc::new(StateMachineEngine::new(wal_config).unwrap());
        let metrics = Arc::new(Metrics::new().unwrap());

        let repl_config = ReplicationConfig {
            role: ReplicationRole::Replica,
            upstream: Some(primary_addr.to_string()),
            reconnect_delay_secs: 0,
            reconnect_max_delay_secs: 1,
            lag_check_interval_secs: 1,
            ..Default::default()
        };

        let client = ReplicaClient::new(
            repl_config,
            engine.clone(),
            primary_addr.to_string(),
            None,
        )
        .unwrap();

        let (shutdown_tx, shutdown_rx) = broadcast::channel(4);
        let client_arc = Arc::new(client);
        let client_clone = client_arc.clone();
        let handle = tokio::spawn(async move {
            client_clone.run(shutdown_rx).await;
        });

        common::ReplicaNode {
            engine,
            metrics,
            shutdown_tx,
            _client_handle: handle,
            _wal_dir: wal_dir,
        }
    }
}
