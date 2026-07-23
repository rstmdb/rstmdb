//! H2 — deterministic tests for the sync-barrier acked-write-loss bug.
//!
//! The sync barrier used to resolve by **sequence**: `await_replication` waited
//! for replicas to ack a sequence >= the WAL head sequence. But `wal.append`
//! assigns sequences via `fetch_add` BEFORE the disk write, so under concurrent
//! writes sequence N+1 can land at a LOWER offset than N. Entries stream (and
//! apply) in offset order, so a replica can ack the higher-sequence/lower-offset
//! write while the lower-sequence/higher-offset write is still missing — yet the
//! sequence-based barrier reports "durable" and the primary acks the client. If
//! the primary then crashes, that write is lost despite sync replication.
//!
//! Reproducing this used to require winning a flaky wall-clock race. Here we
//! make it deterministic with a WAL append hook (feature `test-hooks`): we block
//! the first writer right after it grabs its sequence, let the second writer win
//! the segment-lock race (landing at a lower offset), then release the first.
//! The result is a fixed non-monotonic pair on disk.
//!
//! Fix (manager.rs): the barrier is offset-based. `await_replication` waits for
//! replicas to ack an OFFSET >= the streamed high-water, which durably covers
//! every entry regardless of sequence/offset ordering.
//!
//! Tests:
//!   * `h2_sync_barrier_does_not_lose_nonmonotonic_write` — the barrier must NOT
//!     report success while a lower-sequence/higher-offset write is missing.
//!   * `h2_sync_barrier_converges_when_missing_write_is_acked` — once that write
//!     is finally acked, the barrier resolves (it is not permanently stuck, and
//!     the ack wakes the waiter promptly via `barrier_notify`).
//!   * `h2_concurrent_sync_writes_converge` — under real concurrent sync writes
//!     the offset barrier still resolves and replicas reach parity (the fix does
//!     not break the happy path).

mod common;

use common::{order_machine_def, Cluster, PrimaryOpts, ReplicaOpts};
use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::{Decoder, Encoder};
use rstmdb_server::config::ReplicationMode;
use rstmdb_server::replication::ReplicationMessage;
use rstmdb_wal::WalOffset;
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

// =========================================================================
// Test 1: the barrier must not report a non-durable write as durable.
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h2_sync_barrier_does_not_lose_nonmonotonic_write() {
    let (cluster, ctl, pair) = setup_nonmonotonic_scenario().await;

    // The replica has durably applied B (lower offset) but NOT A (higher offset,
    // withheld). A sequence-based barrier would see the head sequence acked and
    // wrongly report success. The offset-based barrier must refuse.
    let result = cluster.primary.manager.await_replication().await;
    assert!(
        result.is_err(),
        "sync barrier reported success while a lower-sequence/higher-offset \
         write (seq={} offset={}) was NOT durable on the replica — acked-write \
         loss (H2). Result: {:?}",
        pair.seq_a,
        pair.off_a,
        result,
    );

    drop(ctl);
    cluster.shutdown();
}

// =========================================================================
// Test 2: the barrier resolves once the missing write is finally acked.
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h2_sync_barrier_converges_when_missing_write_is_acked() {
    let (cluster, ctl, _pair) = setup_nonmonotonic_scenario().await;

    // Sanity: while the higher-offset write is withheld, the barrier blocks.
    assert!(
        cluster.primary.manager.await_replication().await.is_err(),
        "barrier should block while the higher-offset write is not acked",
    );

    // Now let the replica ack the withheld entry. This advances its offset
    // watermark and wakes the barrier via `barrier_notify`.
    ctl.release_withheld();

    // The barrier must now resolve (everything is durable).
    let result = cluster.primary.manager.await_replication().await;
    assert!(
        result.is_ok(),
        "barrier must resolve once the missing write is acked; got {:?}",
        result,
    );

    drop(ctl);
    cluster.shutdown();
}

// =========================================================================
// Test 3: happy path — real concurrent sync writes still converge.
// =========================================================================

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn h2_concurrent_sync_writes_converge() {
    // A real (fully-acking) replica in sync mode. Hammer the primary with
    // concurrent writes — which genuinely produce non-monotonic sequence/offset
    // pairs on disk — then assert the offset barrier resolves and state matches.
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let primary = Cluster::spawn_primary_with(
        addr,
        PrimaryOpts {
            mode: ReplicationMode::Sync,
            sync_replicas: 1,
            sync_timeout_ms: 5000,
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

    cluster
        .primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();

    // 40 concurrent creates — the WAL segment-lock race makes some sequences
    // land out of order relative to offsets.
    let engine = cluster.primary.engine.clone();
    let mut handles = Vec::new();
    for i in 0..40 {
        let e = engine.clone();
        handles.push(std::thread::spawn(move || {
            e.create_instance(&format!("c-{}", i), "order", 1, json!({ "i": i }), None)
                .unwrap();
        }));
    }
    for h in handles {
        h.join().unwrap();
    }

    // The sync barrier must resolve now that all writes exist and the replica is
    // (or will be) caught up.
    let result = cluster.primary.manager.await_replication().await;
    assert!(
        result.is_ok(),
        "sync barrier must resolve under real concurrent writes; got {:?}",
        result,
    );

    cluster.wait_converged(Duration::from_secs(10)).await;
    cluster.assert_parity();
    for i in 0..40 {
        assert!(
            cluster.replicas[0]
                .engine
                .get_instance(&format!("c-{}", i))
                .is_ok(),
            "replica missing c-{}",
            i
        );
    }

    cluster.shutdown();
}

// =========================================================================
// Helpers
// =========================================================================

/// The non-monotonic pair we engineered on disk.
struct NonMonotonicPair {
    /// Sequence of writer A (the LOWER sequence, HIGHER offset).
    seq_a: u64,
    /// Disk offset of A (higher).
    off_a: u64,
    /// Disk offset of B, which has sequence `seq_a + 1` (lower offset).
    #[allow(dead_code)]
    off_b: u64,
}

/// Stands up a sync-mode primary + a controllable fake replica, then forces a
/// non-monotonic (sequence, offset) pair on the primary and arranges for the
/// replica to withhold the ack for the higher-offset write. Returns the cluster
/// (with `replicas` empty — the fake replica is external), the replica control
/// handle, and the pair metadata.
async fn setup_nonmonotonic_scenario() -> (Cluster, ReplicaControl, NonMonotonicPair) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let primary = Cluster::spawn_primary_with(
        addr,
        PrimaryOpts {
            mode: ReplicationMode::Sync,
            sync_replicas: 1,
            sync_timeout_ms: 400,
            ..Default::default()
        },
    )
    .await;

    let ctl = spawn_controllable_replica(addr).await;

    let cluster = Cluster {
        primary,
        replicas: Vec::new(),
    };
    cluster
        .wait_for_replica_count(1, Duration::from_secs(5))
        .await;

    // Baseline: register the machine and wait for the replica to ack it.
    cluster
        .primary
        .engine
        .put_machine("order", 1, &order_machine_def())
        .unwrap();
    wait_until(Duration::from_secs(5), || {
        cluster
            .primary
            .manager
            .replica_stats()
            .first()
            .map(|(_, acked, _)| *acked >= 1)
            .unwrap_or(false)
    })
    .await;

    // Force the pair, withholding A's ack the instant we learn A's sequence.
    let withheld = ctl.withheld.clone();
    let pair = force_nonmonotonic_pair(cluster.primary.engine.clone(), move |seq_a| {
        withheld.store(seq_a, Ordering::Release);
    })
    .await;

    // Wait until BOTH entries are streamed, so the barrier target is A's higher
    // offset (not just B's).
    let mgr = cluster.primary.manager.clone();
    let off_a = pair.off_a;
    let seq_b = pair.seq_a + 1;
    wait_until(Duration::from_secs(5), || {
        mgr.latest_streamed_offset() >= off_a && mgr.latest_streamed_sequence() >= seq_b
    })
    .await;

    (cluster, ctl, pair)
}

/// Coordination gate between the test and the WAL append hook.
struct Gate {
    state: Mutex<GateState>,
    cv: Condvar,
}
struct GateState {
    entered: Option<u64>,
    released: bool,
}

/// Installs a WAL append hook that blocks the first writer after it grabs its
/// sequence, runs two concurrent writes so the second wins the segment-lock race
/// (lower offset), then releases the first (higher offset). `on_seq_a` is called
/// with writer A's sequence while A is parked — use it to arm any per-sequence
/// test state (e.g. which ack to withhold) before B and the tailer proceed.
async fn force_nonmonotonic_pair(
    engine: Arc<StateMachineEngine>,
    on_seq_a: impl FnOnce(u64),
) -> NonMonotonicPair {
    let gate = Arc::new(Gate {
        state: Mutex::new(GateState {
            entered: None,
            released: false,
        }),
        cv: Condvar::new(),
    });
    let armed = AtomicBool::new(false);
    let gate_hook = gate.clone();
    engine.wal().set_append_hook(Arc::new(move |seq: u64| {
        if !armed.swap(true, Ordering::SeqCst) {
            let mut s = gate_hook.state.lock().unwrap();
            s.entered = Some(seq);
            gate_hook.cv.notify_all();
            while !s.released {
                s = gate_hook.cv.wait(s).unwrap();
            }
        }
    }));

    // Writer A: grabs the lower sequence, then blocks in the hook before writing.
    let eng_a = engine.clone();
    let handle_a = std::thread::spawn(move || {
        eng_a
            .create_instance("na", "order", 1, json!({ "who": "a" }), None)
            .unwrap();
    });

    // Learn A's sequence and let the caller arm per-sequence state.
    let seq_a = {
        let mut s = gate.state.lock().unwrap();
        while s.entered.is_none() {
            s = gate.cv.wait(s).unwrap();
        }
        s.entered.unwrap()
    };
    on_seq_a(seq_a);

    // Writer B: grabs the next sequence and, with A blocked, wins the segment
    // lock — landing at a LOWER offset than A will.
    let eng_b = engine.clone();
    std::thread::spawn(move || {
        eng_b
            .create_instance("nb", "order", 1, json!({ "who": "b" }), None)
            .unwrap();
    })
    .join()
    .unwrap();

    // Release A; it now writes at a HIGHER offset than B.
    {
        let mut s = gate.state.lock().unwrap();
        s.released = true;
        gate.cv.notify_all();
    }
    handle_a.join().unwrap();
    engine.wal().clear_append_hook();

    // Read the actual disk offsets back for each sequence.
    let off_by_seq: HashMap<u64, u64> = engine
        .wal()
        .read_from(WalOffset::from_u64(0), None)
        .unwrap()
        .into_iter()
        .map(|(seq, offset, _)| (seq, offset.as_u64()))
        .collect();
    let off_a = off_by_seq[&seq_a];
    let off_b = off_by_seq[&(seq_a + 1)];

    assert!(
        off_a > off_b,
        "hook failed to invert offset order: seq_a={} off_a={}, seq_b={} off_b={}",
        seq_a,
        off_a,
        seq_a + 1,
        off_b,
    );

    NonMonotonicPair {
        seq_a,
        off_a,
        off_b,
    }
}

/// A hand-built replica that acks every entry EXCEPT the one whose sequence
/// equals `withheld` — which it holds until `release_withheld()` is called.
struct ReplicaControl {
    withheld: Arc<AtomicU64>,
    release: Arc<tokio::sync::Notify>,
    _handle: tokio::task::JoinHandle<()>,
}

impl ReplicaControl {
    /// Tells the replica to ack the entry it has been withholding.
    fn release_withheld(&self) {
        self.release.notify_one();
    }
}

/// Connects a controllable fake replica to `addr` (no auth), completes the
/// handshake, and spawns its selective-ack loop.
async fn spawn_controllable_replica(addr: SocketAddr) -> ReplicaControl {
    let withheld = Arc::new(AtomicU64::new(0)); // 0 = withhold nothing
    let release = Arc::new(tokio::sync::Notify::new());

    let mut stream = TcpStream::connect(addr).await.unwrap();
    let auth = ReplicationMessage::ReplicateAuth {
        auth_token: None,
        last_sequence: 0,
        last_primary_offset: 0,
    };
    stream
        .write_all(&Encoder::encode_raw(&auth.to_bytes().unwrap()))
        .await
        .unwrap();

    // Read the sync response before entering the ack loop.
    let mut decoder = Decoder::new();
    let mut buf = [0u8; 16384];
    loop {
        let n = stream.read(&mut buf).await.unwrap();
        assert!(n > 0, "primary closed before sync response");
        decoder.extend(&buf[..n]);
        if let Ok(Some(payload)) = decoder.decode_raw() {
            match ReplicationMessage::from_bytes(&payload).unwrap() {
                ReplicationMessage::ReplicateSyncResponse { ok: true, .. } => break,
                other => panic!("unexpected handshake response: {:?}", other),
            }
        }
    }

    let (mut rd, mut wr) = stream.into_split();
    let withheld_task = withheld.clone();
    let release_task = release.clone();
    let handle = tokio::spawn(async move {
        // The single entry we are currently withholding, if any.
        let mut held: Option<(u64, u64)> = None;
        loop {
            tokio::select! {
                r = rd.read(&mut buf) => {
                    let n = match r { Ok(0) => break, Ok(n) => n, Err(_) => break };
                    decoder.extend(&buf[..n]);
                    while let Ok(Some(payload)) = decoder.decode_raw() {
                        if let Ok(ReplicationMessage::ReplicateEntry { sequence, offset, .. }) =
                            ReplicationMessage::from_bytes(&payload)
                        {
                            if sequence == withheld_task.load(Ordering::Acquire) {
                                held = Some((sequence, offset)); // hold, don't ack yet
                            } else if send_ack(&mut wr, sequence, offset).await.is_err() {
                                return;
                            }
                        }
                    }
                }
                _ = release_task.notified() => {
                    if let Some((seq, off)) = held.take() {
                        if send_ack(&mut wr, seq, off).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });

    ReplicaControl {
        withheld,
        release,
        _handle: handle,
    }
}

async fn send_ack<W: AsyncWriteExt + Unpin>(
    wr: &mut W,
    sequence: u64,
    applied_offset: u64,
) -> std::io::Result<()> {
    let ack = ReplicationMessage::ReplicateAck {
        sequence,
        applied_offset,
    };
    wr.write_all(&Encoder::encode_raw(&ack.to_bytes().unwrap()))
        .await
}

/// Polls `cond` until it returns true or the timeout elapses (then panics).
async fn wait_until<F: FnMut() -> bool>(timeout: Duration, mut cond: F) {
    let start = Instant::now();
    while start.elapsed() < timeout {
        if cond() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not met within {:?}", timeout);
}
