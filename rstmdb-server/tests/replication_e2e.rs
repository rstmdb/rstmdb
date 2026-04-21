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

use common::{order_machine_def, Cluster, PrimaryOpts, ReplicaOpts, ReplicaNode};
use rstmdb_server::config::ReplicationMode;
use serde_json::json;
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
    let late_replica = Cluster::spawn_replica_with(primary_addr, Default::default()).await;
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
// Reconnect: replica disconnects and reconnects, missed writes catch up
// =========================================================================

#[tokio::test]
async fn e2e_replica_reconnects_after_shutdown() {
    // Start a primary with 2 replicas, write some data, then cleanly shut
    // down one replica's client task. Write more data. Start a fresh replica
    // pointing at the same primary. The fresh replica must catch up.
    let mut cluster = Cluster::spawn(2).await;

    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    for i in 0..5 {
        cluster
            .primary
            .engine
            .create_instance(&format!("pre-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }
    cluster.wait_converged(Duration::from_secs(3)).await;

    // Shut down replica #1 (its client task exits; primary sees disconnect)
    let dead = cluster.replicas.remove(1);
    let _ = dead.shutdown_tx.send(());
    // Wait until the primary no longer sees it
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;

    // Write more data while only 1 replica is connected
    for i in 0..5 {
        cluster
            .primary
            .engine
            .create_instance(&format!("mid-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }
    cluster.wait_converged(Duration::from_secs(3)).await;

    // Spawn a replacement replica — must catch up everything
    let primary_addr = cluster.primary.addr;
    cluster
        .replicas
        .push(Cluster::spawn_replica_with(primary_addr, ReplicaOpts::default()).await);
    cluster
        .wait_for_replica_count(2, Duration::from_secs(5))
        .await;
    cluster.wait_converged(Duration::from_secs(5)).await;
    cluster.assert_parity();

    cluster.shutdown();
}

#[tokio::test]
async fn e2e_replica_reconnects_via_reconnect_loop() {
    // Simulate transient failure: the primary is up, but the replica client
    // encounters connection close and auto-reconnects (exponential backoff).
    // Writes that happen during the gap are recovered via catch-up.
    let mut cluster = Cluster::spawn(1).await;
    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    cluster.wait_converged(Duration::from_secs(3)).await;

    // Take the single replica down, write many entries, bring up a fresh one
    let dead = cluster.replicas.remove(0);
    let _ = dead.shutdown_tx.send(());
    cluster
        .wait_for_replica_count(0, Duration::from_secs(5))
        .await;

    for i in 0..30 {
        cluster
            .primary
            .engine
            .create_instance(&format!("gap-{}", i), "order", 1, json!({"i": i}), None)
            .unwrap();
    }

    // Bring up a new replica — it must catch up ALL 30 instances + the machine
    let primary_addr = cluster.primary.addr;
    cluster
        .replicas
        .push(Cluster::spawn_replica_with(primary_addr, ReplicaOpts::default()).await);
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;
    cluster.wait_converged(Duration::from_secs(5)).await;

    let r = &cluster.replicas[0];
    assert!(r.engine.get_machine("order", 1).is_ok());
    for i in 0..30 {
        assert!(
            r.engine.get_instance(&format!("gap-{}", i)).is_ok(),
            "reconnected replica missing gap-{}",
            i
        );
    }

    cluster.shutdown();
}

// =========================================================================
// Lag: metrics reflect actual state
// =========================================================================

#[tokio::test]
async fn e2e_lag_zero_when_caught_up() {
    let cluster = Cluster::spawn(2).await;
    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    for i in 0..10 {
        cluster
            .primary
            .engine
            .create_instance(&format!("i-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }
    cluster.wait_converged(Duration::from_secs(3)).await;

    // Primary-side view: per-replica lag should be 0
    let stats = cluster.primary.manager.replica_stats();
    assert_eq!(stats.len(), 2);
    for (id, acked, lag) in &stats {
        assert_eq!(*lag, 0, "replica {} has non-zero lag ({})", id, lag);
        assert!(*acked > 0, "replica {} should have acked entries", id);
    }

    // Replica-side view: each replica's own lag should be 0
    for r in &cluster.replicas {
        assert_eq!(r.client.lag_entries(), 0);
    }

    cluster.shutdown();
}

#[tokio::test]
async fn e2e_lag_increases_then_recovers() {
    // While one replica is disconnected, the primary keeps writing. When the
    // replica comes back, the primary-side per-replica stats should briefly
    // show lag, then go to 0 after catch-up completes.
    let mut cluster = Cluster::spawn(1).await;
    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    cluster.wait_converged(Duration::from_secs(3)).await;

    // Take the replica down
    let dead = cluster.replicas.remove(0);
    let _ = dead.shutdown_tx.send(());
    cluster
        .wait_for_replica_count(0, Duration::from_secs(5))
        .await;

    // Primary writes while no replica is connected
    for i in 0..40 {
        cluster
            .primary
            .engine
            .create_instance(&format!("lag-{}", i), "order", 1, json!({"n": i}), None)
            .unwrap();
    }

    // Spawn a fresh replica — it catches up from scratch
    let primary_addr = cluster.primary.addr;
    cluster
        .replicas
        .push(Cluster::spawn_replica_with(primary_addr, ReplicaOpts::default()).await);
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;

    // Eventually lag must reach 0
    cluster.wait_converged(Duration::from_secs(10)).await;
    let stats = cluster.primary.manager.replica_stats();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].2, 0, "lag should be 0 after catch-up");

    cluster.shutdown();
}

// =========================================================================
// Primary-side observability
// =========================================================================

#[tokio::test]
async fn e2e_primary_sees_per_replica_sequence() {
    let cluster = Cluster::spawn(3).await;
    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    for i in 0..10 {
        cluster
            .primary
            .engine
            .create_instance(&format!("r-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }
    cluster.wait_converged(Duration::from_secs(5)).await;

    let stats = cluster.primary.manager.replica_stats();
    assert_eq!(stats.len(), 3);
    let primary_seq = cluster.primary.engine.wal().next_sequence().saturating_sub(1);
    for (id, acked, lag) in &stats {
        assert_eq!(*lag, 0, "replica {} lag should be 0", id);
        assert_eq!(*acked, primary_seq, "replica {} acked sequence mismatch", id);
    }

    cluster.shutdown();
}

#[tokio::test]
async fn e2e_connected_replica_count_updates_on_disconnect() {
    let mut cluster = Cluster::spawn(3).await;
    assert_eq!(cluster.primary.manager.connected_replica_count(), 3);

    let dead = cluster.replicas.pop().unwrap();
    let _ = dead.shutdown_tx.send(());
    cluster
        .wait_for_replica_count(2, Duration::from_secs(5))
        .await;
    assert_eq!(cluster.primary.manager.connected_replica_count(), 2);

    let dead2 = cluster.replicas.pop().unwrap();
    let _ = dead2.shutdown_tx.send(());
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;
    assert_eq!(cluster.primary.manager.connected_replica_count(), 1);

    cluster.shutdown();
}

// =========================================================================
// Auth: token validation
// =========================================================================

#[tokio::test]
async fn e2e_auth_correct_plaintext_token_accepted() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let primary = Cluster::spawn_primary_with(
        addr,
        PrimaryOpts {
            auth_token: Some("secret123".to_string()),
            ..Default::default()
        },
    )
    .await;

    let replica = Cluster::spawn_replica_with(
        addr,
        ReplicaOpts {
            auth_token: Some("secret123".to_string()),
            ..Default::default()
        },
    )
    .await;

    let cluster = Cluster {
        primary,
        replicas: vec![replica],
    };
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;

    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    cluster.wait_converged(Duration::from_secs(3)).await;

    assert!(cluster.replicas[0].engine.get_machine("order", 1).is_ok());

    cluster.shutdown();
}

#[tokio::test]
async fn e2e_auth_wrong_token_rejected() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let primary = Cluster::spawn_primary_with(
        addr,
        PrimaryOpts {
            auth_token: Some("correct-token".to_string()),
            ..Default::default()
        },
    )
    .await;

    // Replica with WRONG token
    let replica = Cluster::spawn_replica_with(
        addr,
        ReplicaOpts {
            auth_token: Some("wrong-token".to_string()),
            ..Default::default()
        },
    )
    .await;

    // Replica will keep reconnecting, but the primary must reject
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        primary.manager.connected_replica_count(),
        0,
        "primary must reject replica with wrong token"
    );

    let _ = primary.shutdown_tx.send(());
    let _ = replica.shutdown_tx.send(());
}

#[tokio::test]
async fn e2e_auth_hashed_token_accepted() {
    use rstmdb_server::auth::TokenValidator;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let hash = TokenValidator::hash_token("rotation-token");
    let primary = Cluster::spawn_primary_with(
        addr,
        PrimaryOpts {
            auth_token_hashes: vec![hash],
            ..Default::default()
        },
    )
    .await;

    let replica = Cluster::spawn_replica_with(
        addr,
        ReplicaOpts {
            auth_token: Some("rotation-token".to_string()),
            ..Default::default()
        },
    )
    .await;

    let cluster = Cluster {
        primary,
        replicas: vec![replica],
    };
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;

    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    cluster.wait_converged(Duration::from_secs(3)).await;

    assert!(cluster.replicas[0].engine.get_machine("order", 1).is_ok());

    cluster.shutdown();
}

// =========================================================================
// Sync mode: writes wait for ACKs
// =========================================================================

#[tokio::test]
async fn e2e_sync_mode_waits_for_ack() {
    // This test exercises `await_replication`. We use the manager directly
    // since the engine's write path doesn't go through the server here.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let primary = Cluster::spawn_primary_with(
        addr,
        PrimaryOpts {
            mode: ReplicationMode::Sync,
            sync_replicas: 1,
            sync_timeout_ms: 2000,
            ..Default::default()
        },
    )
    .await;

    let replica = Cluster::spawn_replica_with(addr, ReplicaOpts::default()).await;

    let cluster = Cluster {
        primary,
        replicas: vec![replica],
    };
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;

    // Write something — the tailer will fan out and replica will ACK
    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    cluster.primary.engine.create_instance("x", "order", 1, json!({}), None).unwrap();

    // Wait for replica to apply (so ACK is sent)
    cluster.wait_converged(Duration::from_secs(3)).await;

    // Now await_replication should succeed quickly (replica is up to date)
    let result = cluster.primary.manager.await_replication().await;
    assert!(result.is_ok(), "sync replication should succeed: {:?}", result);

    cluster.shutdown();
}

#[tokio::test]
async fn e2e_sync_mode_times_out_without_replicas() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let primary = Cluster::spawn_primary_with(
        addr,
        PrimaryOpts {
            mode: ReplicationMode::Sync,
            sync_replicas: 1,
            sync_timeout_ms: 100,
            ..Default::default()
        },
    )
    .await;

    // Write without any replica connected
    primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();

    let result = primary.manager.await_replication().await;
    assert!(result.is_err(), "sync replication must fail with no replicas");
    let msg = result.unwrap_err();
    assert!(
        msg.contains("no replicas") || msg.contains("timeout"),
        "unexpected error: {}",
        msg
    );

    let _ = primary.shutdown_tx.send(());
}

// =========================================================================
// Persistence: replica WAL survives a simulated restart
// =========================================================================

#[tokio::test]
async fn e2e_replica_wal_persists_across_restart() {
    // Use a persistent tempdir for the replica so we can "restart" it
    // (shutdown + spawn new one on same dir) and verify state is recovered.
    let mut cluster = Cluster::spawn(0).await;

    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    for i in 0..5 {
        cluster
            .primary
            .engine
            .create_instance(&format!("persist-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }

    // Spawn a replica with an explicit wal_dir we control
    let persistent_wal = tempfile::TempDir::new().unwrap();
    let primary_addr = cluster.primary.addr;
    let replica = Cluster::spawn_replica_with(
        primary_addr,
        ReplicaOpts {
            wal_dir: Some(persistent_wal.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await;
    cluster.replicas.push(replica);
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;
    cluster.wait_converged(Duration::from_secs(5)).await;

    // Sanity: replica has everything
    for i in 0..5 {
        assert!(cluster.replicas[0]
            .engine
            .get_instance(&format!("persist-{}", i))
            .is_ok());
    }

    // "Restart" the replica: shutdown, drop, respawn on same dir
    let dead = cluster.replicas.pop().unwrap();
    let _ = dead.shutdown_tx.send(());
    drop(dead);
    cluster
        .wait_for_replica_count(0, Duration::from_secs(5))
        .await;

    // Primary writes while replica is "restarting"
    cluster
        .primary
        .engine
        .create_instance("after-restart", "order", 1, json!({}), None)
        .unwrap();

    // Bring the replica back on the SAME dir — it should recover pre-restart
    // state from local WAL and then catch up the new entry from primary.
    let replica2 = Cluster::spawn_replica_with(
        primary_addr,
        ReplicaOpts {
            wal_dir: Some(persistent_wal.path().to_path_buf()),
            ..Default::default()
        },
    )
    .await;
    cluster.replicas.push(replica2);
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;
    cluster.wait_converged(Duration::from_secs(5)).await;

    // All pre-restart data must still be there
    for i in 0..5 {
        assert!(cluster.replicas[0]
            .engine
            .get_instance(&format!("persist-{}", i))
            .is_ok());
    }
    // And the new entry
    assert!(cluster.replicas[0]
        .engine
        .get_instance("after-restart")
        .is_ok());

    cluster.shutdown();
}

// =========================================================================
// Idempotency: same idempotency_key across writes
// =========================================================================

#[tokio::test]
async fn e2e_idempotency_keys_work_under_replication() {
    let cluster = Cluster::spawn(1).await;

    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    cluster
        .primary
        .engine
        .create_instance("idem", "order", 1, json!({}), None)
        .unwrap();

    // Apply same event twice with same idempotency_key — should only apply once
    let r1 = cluster
        .primary
        .engine
        .apply_event("idem", "PAY", json!({"n": 1}), None, None, None, Some("k1"))
        .unwrap();
    let r2 = cluster
        .primary
        .engine
        .apply_event("idem", "PAY", json!({"n": 2}), None, None, None, Some("k1"))
        .unwrap();
    assert_eq!(r1.wal_offset, r2.wal_offset);
    assert_eq!(r1.to_state, r2.to_state);

    cluster.wait_converged(Duration::from_secs(3)).await;

    // Replica should see the applied state only once
    let replica_inst = cluster.replicas[0].engine.get_instance("idem").unwrap();
    assert_eq!(replica_inst.state, "paid");
    assert_eq!(replica_inst.ctx["n"], 1);

    cluster.shutdown();
}

// =========================================================================
// Stress: many interleaved writes, multiple replicas converge
// =========================================================================

#[tokio::test]
async fn e2e_mixed_workload_multiple_replicas() {
    let cluster = Cluster::spawn(3).await;
    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();

    // Mix of creates and events
    let engine = cluster.primary.engine.clone();
    let mut tasks = Vec::new();
    for i in 0..30 {
        let e = engine.clone();
        tasks.push(tokio::spawn(async move {
            let id = format!("w-{}", i);
            e.create_instance(&id, "order", 1, json!({}), None).unwrap();
            e.apply_event(&id, "PAY", json!({"n": i}), None, None, None, None)
                .unwrap();
            if i % 2 == 0 {
                e.apply_event(&id, "SHIP", json!({}), None, None, None, None)
                    .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }

    cluster.wait_converged(Duration::from_secs(10)).await;
    cluster.assert_parity();

    // Spot-check: shipped items on each replica
    for r in &cluster.replicas {
        for i in (0..30).step_by(2) {
            let id = format!("w-{}", i);
            let inst = r.engine.get_instance(&id).unwrap();
            assert_eq!(inst.state, "shipped");
        }
    }

    cluster.shutdown();
}

// =========================================================================
// Catch-up correctness: late replica receives EXACTLY what primary has
// =========================================================================

#[tokio::test]
async fn e2e_catchup_wal_entry_count_exact() {
    let mut cluster = Cluster::spawn(0).await;

    // Prewrite data without any replica connected
    cluster.primary.engine.put_machine("order", 1, &order_machine_def()).unwrap();
    for i in 0..25 {
        cluster
            .primary
            .engine
            .create_instance(&format!("pre-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }

    let expected = cluster.primary.engine.wal().next_sequence().saturating_sub(1);
    assert_eq!(expected, 26); // 1 put_machine + 25 creates

    // Now attach a replica — it must catch up ALL entries
    let primary_addr = cluster.primary.addr;
    let replica: ReplicaNode =
        Cluster::spawn_replica_with(primary_addr, ReplicaOpts::default()).await;
    cluster.replicas.push(replica);
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;
    cluster.wait_converged(Duration::from_secs(5)).await;

    let replica_entries = cluster.replicas[0]
        .engine
        .wal()
        .next_sequence()
        .saturating_sub(1);
    assert_eq!(
        replica_entries, expected,
        "replica WAL entries must exactly match primary ({} vs {})",
        replica_entries, expected
    );

    cluster.shutdown();
}
