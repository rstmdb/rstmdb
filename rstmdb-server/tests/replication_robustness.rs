//! Robustness tests for the two Medium replication findings (the fuzz-adjacent
//! codec / detection corners from the brainstorm).
//!
//!   M2 — a WAL-accepted entry near the record-size limit, once wrapped in a
//!        `ReplicateEntry` JSON envelope, exceeded the wire-frame limit and
//!        panicked `Encoder::encode_raw` (a write-triggered DoS on the primary's
//!        replication tasks). The record and frame limits were both 16 MiB, so
//!        the envelope pushed a max entry over the edge.
//!        Fix: `MAX_RECORD_SIZE` now reserves headroom below `MAX_PAYLOAD_SIZE`,
//!        so any WAL-accepted entry always frames within one wire frame.
//!
//!   M4 — replication detection did a single `read()` then decoded once; a
//!        handshake split across TCP segments decoded as incomplete and the
//!        replica was misrouted to the client handler.
//!        Fix: the detector reads in a bounded loop until a complete first frame
//!        decodes.

mod common;

use common::{Cluster, PrimaryOpts};
use rstmdb_protocol::{Encoder, MAX_PAYLOAD_SIZE};
use rstmdb_server::replication::ReplicationMessage;
use rstmdb_wal::entry::MAX_RECORD_SIZE;
use rstmdb_wal::WalEntry;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};

// =========================================================================
// M2 — a max-size WAL entry must always fit inside a replication wire frame.
//
// `Encoder::encode_raw` panics (`.expect`) on payloads > MAX_PAYLOAD_SIZE. The
// replication tasks frame every WAL entry as a `ReplicateEntry` JSON message, so
// the largest entry the WAL accepts, plus that envelope, must stay within the
// frame limit — otherwise streaming it panics the tailer/writer/catch-up task.
// =========================================================================

#[test]
fn m2_max_wal_entry_fits_replication_frame() {
    // Measure the worst-case ReplicateEntry envelope: max-width numeric fields
    // (u64::MAX / u32::MAX are the widest decimal encodings).
    let entry = WalEntry::CreateInstance {
        instance_id: "x".to_string(),
        machine: "m".to_string(),
        version: u32::MAX,
        initial_state: "s".to_string(),
        initial_ctx: json!({}),
        idempotency_key: None,
    };
    let entry_json_len = serde_json::to_vec(&entry).unwrap().len();

    let msg = ReplicationMessage::ReplicateEntry {
        sequence: u64::MAX,
        offset: u64::MAX,
        entry,
        timestamp_ms: u64::MAX,
    };
    let msg_json_len = serde_json::to_vec(&msg).unwrap().len();

    // The envelope is everything the ReplicateEntry wrapper adds around the
    // embedded entry JSON (type tag + sequence/offset/timestamp + structure).
    let envelope = msg_json_len - entry_json_len;

    // The invariant: a max-size WAL record + envelope must fit one wire frame.
    // If this fails, an entry sized right at MAX_RECORD_SIZE would be accepted
    // by the WAL but panic encode_raw when the primary streamed it (M2).
    assert!(
        MAX_RECORD_SIZE + envelope <= MAX_PAYLOAD_SIZE as usize,
        "max WAL record ({}) + replication envelope ({}) = {} exceeds the wire \
         frame limit ({}) — streaming a max-size entry would panic encode_raw",
        MAX_RECORD_SIZE,
        envelope,
        MAX_RECORD_SIZE + envelope,
        MAX_PAYLOAD_SIZE,
    );
}

// =========================================================================
// M4 — a replication handshake split across TCP segments must still be
// detected as a replication connection (not misrouted to the client handler).
// =========================================================================

#[tokio::test]
async fn m4_segmented_handshake_is_detected_as_replication() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let primary = Cluster::spawn_primary_with(addr, PrimaryOpts::default()).await;

    let auth = ReplicationMessage::ReplicateAuth {
        auth_token: None,
        last_sequence: 0,
        last_primary_offset: 0,
    };
    let frame = Encoder::encode_raw(&auth.to_bytes().unwrap());
    assert!(
        frame.len() > 4,
        "handshake frame too small to split meaningfully"
    );

    // Send the handshake frame in two writes with a gap, so the server's first
    // read() sees only a partial frame — exactly what TCP segmentation does.
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let mid = frame.len() / 2;
    stream.write_all(&frame[..mid]).await.unwrap();
    stream.flush().await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    stream.write_all(&frame[mid..]).await.unwrap();
    stream.flush().await.unwrap();

    // The primary must recognize the replica despite the split. Under the old
    // single-read detection this connection was treated as a client and never
    // counted as a replica.
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(5) {
        if primary.manager.connected_replica_count() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        primary.manager.connected_replica_count(),
        1,
        "a segmented ReplicateAuth handshake was not detected as a replication \
         connection (M4)",
    );

    let _ = primary.shutdown_tx.send(());
}
