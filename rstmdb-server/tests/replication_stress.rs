//! Adversarial stress tests for the replication design bets in PR #8.
//!
//! Companion to `replication_e2e.rs`. Where the e2e suite proves the happy
//! paths converge, this suite probes the *failure* paths that a deep code
//! review flagged as High-severity. Each test asserts the **correct** (secure /
//! non-diverging) behavior. They were written RED against this branch to
//! demonstrate the bugs, then went GREEN with the fixes and now stand as
//! regression guards.
//!
//! Findings covered:
//!   H1 — replication bypassed client auth (security / side-door).
//!        Fix: ReplicationManager falls back to the client-auth validator when
//!        replication auth is unset (manager.rs).
//!   H3 — replica silently dropped entries and stayed connected when its WAL
//!        apply failed. Fix: the replica tears down the connection on apply
//!        error instead of skipping and continuing (replica_client.rs).
//!   H4 — catch-up livelock under load: a catching-up replica was disconnected
//!        as "slow" because the channel drainer only spawned after catch-up.
//!        Fix: catch up from the WAL by offset cursor WITHOUT joining the
//!        fan-out map; join (and slow-live detection) only once caught up.
//!
//! H2 (sync barrier resolves by non-monotonic sequence → acked-write loss) is
//! covered by `replication_h2_barrier.rs`, which uses a test-only interleaving
//! hook.

mod common;

use common::order_machine_def;
use common::{probe_replication_stream, Cluster, PrimaryOpts, ReplicaOpts};
use rstmdb_server::auth::TokenValidator;
use rstmdb_server::replication::ReplicationMessage;
use serde_json::json;
use std::time::Duration;
use tokio::net::TcpListener;

// =========================================================================
// H1 — Auth side-door (design bet S5)
//
// A primary with CLIENT auth ON but REPLICATION auth UNSET must not hand its
// WAL to an anonymous TCP peer. These are two independent config knobs
// (`auth.required` vs `replication.auth_*`); the replication dispatch path
// never consults the client-auth handshake, and when replication auth is
// unset there is no token validator at all — so today the peer gets a
// `ReplicateSyncResponse{ok:true}` followed by the entire WAL as catch-up.
//
// This is the one finding that is a security hole rather than a tail-latency
// corner: full database exfiltration with a single hand-built frame.
// =========================================================================

#[tokio::test]
async fn h1_unauthenticated_peer_cannot_exfiltrate_wal_when_client_auth_on() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    // Operator turns on client auth and reasonably assumes the server is now
    // locked down — but forgets to set a replication token.
    let primary = Cluster::spawn_primary_with(
        addr,
        PrimaryOpts {
            client_auth_required: true,
            client_auth_token_hashes: vec![TokenValidator::hash_token("client-secret")],
            // Replication auth intentionally left unset (the trap).
            auth_token: None,
            auth_token_hashes: Vec::new(),
            ..Default::default()
        },
    )
    .await;

    // Put some sensitive data in the primary's WAL.
    primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();
    primary
        .engine
        .create_instance(
            "secret-instance",
            "order",
            1,
            json!({ "pii": "ssn-123-45-6789" }),
            None,
        )
        .unwrap();

    // Adversary: raw TCP + one ReplicateAuth frame carrying NO token.
    let probe = probe_replication_stream(addr, None, Duration::from_millis(500)).await;

    // SECURE behavior (fails today): not a single WAL entry may be streamed to
    // an anonymous peer.
    let leaked: Vec<&ReplicationMessage> = probe
        .entries
        .iter()
        .filter(|m| matches!(m, ReplicationMessage::ReplicateEntry { .. }))
        .collect();
    assert!(
        leaked.is_empty(),
        "SECURITY: primary streamed {} WAL entries to an unauthenticated peer \
         — full database exfiltration via the replication side-door",
        leaked.len(),
    );

    // ...and the handshake itself must be rejected.
    assert_ne!(
        probe.sync_ok,
        Some(true),
        "SECURITY: primary with client auth ON accepted an unauthenticated \
         replication handshake (sync_ok=true, error={:?})",
        probe.sync_error,
    );

    let _ = primary.shutdown_tx.send(());
}

// =========================================================================
// H3 — Silent divergence on replica apply failure (design bet S6)
//
// When `apply_replicated_entry` fails, the replica logs an error and
// *continues the stream loop* (replica_client.rs): it doesn't ACK the entry,
// doesn't advance its offset, and — critically — doesn't disconnect. The
// primary keeps counting it as a healthy, connected replica while every
// subsequent write silently vanishes on the replica side. Because the next
// successful apply fetch_max's the offset past the gap, a reconnect never
// re-sends the skipped entry either: the divergence is permanent.
//
// NOTE ON REPRODUCTION: the brainstorm proposed a "state-level poison entry."
// That path turns out to be unreachable — `replay_entry` is infallible for
// every WalEntry variant, and the 16 MiB wire-frame limit equals the WAL
// record limit, so a too-large entry is rejected by the frame decoder before
// it ever reaches the WAL. The only way to make `apply_replicated_entry`
// return Err is a genuine replica-side WAL I/O failure, which we inject here
// by closing the replica's WAL (every later append returns WalError::Closed).
// =========================================================================

#[tokio::test]
async fn h3_replica_disconnects_when_wal_apply_fails() {
    // Long reconnect delay: once the replica disconnects on apply failure it
    // stays gone for the rest of the test, so `wait_for_replica_count(0)` is a
    // deterministic signal in both directions — with the fix the replica drops
    // and stays at 0; with the bug it never disconnects and the wait times out.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let primary = Cluster::spawn_primary_with(addr, PrimaryOpts::default()).await;
    let replica = Cluster::spawn_replica_with(
        addr,
        ReplicaOpts {
            reconnect_delay_secs: 30,
            reconnect_max_delay_secs: 60,
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

    // Baseline: one entry replicates cleanly so we know the link is healthy.
    cluster
        .primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();
    cluster.wait_converged(Duration::from_secs(3)).await;
    assert!(cluster.replicas[0].engine.get_machine("order", 1).is_ok());

    // Inject a replica-side WAL I/O fault: from now on every
    // apply_replicated_entry -> wal.append returns WalError::Closed, driving
    // the replica into its apply-error branch for each streamed entry.
    cluster.replicas[0].engine.wal().close().unwrap();

    // Primary keeps writing (well under the 4096 channel capacity, so this is
    // NOT a slow-replica backpressure disconnect — it isolates apply failure).
    for i in 0..10 {
        cluster
            .primary
            .engine
            .create_instance(&format!("after-fail-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }

    // Sanity: the replica genuinely cannot persist the new entries.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        cluster.replicas[0]
            .engine
            .get_instance("after-fail-0")
            .is_err(),
        "precondition: replica should have failed to apply post-fault entries",
    );

    // CORRECT behavior (fails today): a replica that can no longer persist
    // entries must surface the failure and drop out of the fleet, not silently
    // masquerade as a healthy connected replica while dropping every write.
    cluster
        .wait_for_replica_count(0, Duration::from_secs(5))
        .await;

    cluster.shutdown();
}

// =========================================================================
// H4 — Catch-up livelock under load (design bet S3)
//
// A fresh replica joins with a backlog to catch up while the primary keeps
// taking heavy writes. In the old code the replica joined the fan-out map
// BEFORE catch-up, and the channel drainer (writer task) only spawned AFTER
// catch-up — so live writes during the catch-up window overflowed the bounded
// per-replica channel (4096) and the tailer disconnected the replica as
// "slow". On reconnect it restarted catch-up and hit the same wall: livelock.
//
// Fix: a replica catches up straight from the WAL by offset cursor WITHOUT
// joining the fan-out map, so live writes can't overflow its channel and it is
// never spuriously disconnected during catch-up. It joins the map (and the
// slow-LIVE-replica detection) only once caught up. The tailer also advances
// its cursor while the map is empty, so a lone replica joining post-catch-up
// doesn't trigger a full-history replay.
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h4_catchup_under_load_does_not_livelock() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let primary = Cluster::spawn_primary_with(addr, PrimaryOpts::default()).await;

    // Pre-write a backlog so catch-up takes a meaningful window.
    primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();
    for i in 0..5000 {
        primary
            .engine
            .create_instance(&format!("backlog-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }

    // Attach the replica; it begins catching up the backlog.
    let replica = Cluster::spawn_replica_with(addr, ReplicaOpts::default()).await;
    let cluster = Cluster {
        primary,
        replicas: vec![replica],
    };

    // Sustain heavy writes DURING catch-up, on a separate thread so they run
    // concurrently with the replica's catch-up. This is > REPLICA_CHANNEL_CAPACITY
    // (4096): under the old code these overflow the catching-up replica's channel
    // and disconnect it as "slow"; under the fix the replica isn't in the fan-out
    // during catch-up, so they can't.
    let eng = cluster.primary.engine.clone();
    std::thread::spawn(move || {
        for i in 0..6000 {
            eng.create_instance(&format!("live-{}", i), "order", 1, json!({}), None)
                .unwrap();
        }
    })
    .join()
    .unwrap();

    // The replica must converge (the livelock would prevent this) ...
    cluster.wait_converged(Duration::from_secs(30)).await;

    // ... without ever being disconnected as "slow" during catch-up.
    let slow = cluster
        .primary
        .metrics
        .replication_slow_replica_disconnects_total
        .get();
    assert_eq!(
        slow, 0.0,
        "replica was spuriously disconnected as slow during catch-up under load (H4 livelock)",
    );

    cluster.assert_parity();
    cluster.shutdown();
}
