//! A running server must not acknowledge writes once its WAL segment is no
//! longer the file at its path (issue #19).
//!
//! Each test starts a real server on a free local port (never the live 7401)
//! with a temp data dir, then deletes or replaces the WAL under it.

use rstmdb_client::{Client, ClientError, ConnectionConfig};
use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::ErrorCode;
use rstmdb_server::{Server, ServerConfig};
use rstmdb_storage::SnapshotStore;
use rstmdb_wal::{FsyncPolicy, WalConfig};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tempfile::TempDir;
use tokio::task::JoinHandle;

const LIVE_PORT: u16 = 7401;

struct Node {
    server: Arc<Server>,
    engine: Arc<StateMachineEngine>,
    handle: JoinHandle<()>,
    client: Client,
}

async fn start(data_dir: &Path, segment_bytes: u64) -> Node {
    let addr: SocketAddr = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.local_addr().unwrap()
    };
    assert_ne!(
        addr.port(),
        LIVE_PORT,
        "test must not use the live server port"
    );

    let wal_config = WalConfig::new(data_dir.join("wal"))
        .with_segment_size(segment_bytes)
        .with_fsync_policy(FsyncPolicy::EveryWrite);
    let snapshot_dir = data_dir.join("snapshots");
    let store = SnapshotStore::open(&snapshot_dir).unwrap();
    let recovery = store.load_recovery_state().unwrap();
    drop(store);
    let engine = Arc::new(StateMachineEngine::new_with_recovery(wal_config, recovery).unwrap());
    let server = Arc::new(
        Server::with_snapshots(ServerConfig::new(addr), engine.clone(), &snapshot_dir).unwrap(),
    );
    let handle = {
        let server = server.clone();
        tokio::spawn(async move {
            let _ = server.run().await;
        })
    };
    let start = Instant::now();
    while tokio::net::TcpStream::connect(addr).await.is_err() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "server did not start"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let client = Client::new(ConnectionConfig::new(addr));
    client.connect().await.unwrap();
    let conn = client.connection();
    tokio::spawn(async move {
        let _ = conn.read_loop().await;
    });
    Node {
        server,
        engine,
        handle,
        client,
    }
}

async fn stop(node: Node) {
    let _ = node.client.close().await;
    node.server.shutdown();
    tokio::time::timeout(Duration::from_secs(5), node.handle)
        .await
        .expect("server did not stop")
        .unwrap();
}

fn counter_machine() -> Value {
    json!({
        "states": ["open"],
        "initial": "open",
        "transitions": [{"from": "open", "event": "TICK", "to": "open"}]
    })
}

/// Machine, instance and `events` acknowledged TICKs.
async fn seed(client: &Client, events: u64) {
    client
        .put_machine("counter", 1, counter_machine())
        .await
        .unwrap();
    client
        .create_instance("counter", 1, Some("c-1"), Some(json!({})), None)
        .await
        .unwrap();
    for n in 1..=events {
        client
            .apply_event("c-1", "TICK", Some(json!({"n": n})), None, None)
            .await
            .unwrap();
    }
}

/// The next write is refused, with a non-retryable WAL error naming `path`.
async fn assert_write_refused(client: &Client, path: &Path, reason: &str) {
    let err = client
        .apply_event("c-1", "TICK", Some(json!({"n": 999})), None, None)
        .await
        .expect_err("a write into a detached WAL must be refused");
    match err {
        ClientError::ServerError {
            code,
            message,
            retryable,
        } => {
            assert_eq!(code, ErrorCode::WalIoError);
            assert!(!retryable, "a detached WAL is not a transient error");
            assert!(message.contains(&path.display().to_string()), "{}", message);
            assert!(message.contains(reason), "{}", message);
        }
        other => panic!("expected a server error, got {:?}", other),
    }
}

async fn wal_info(client: &Client) -> Value {
    client.info().await.unwrap()["wal"].clone()
}

#[tokio::test]
async fn deleting_the_wal_directory_makes_the_server_refuse_writes() {
    let data = TempDir::new().unwrap();
    let node = start(data.path(), 64 * 1024).await;
    seed(&node.client, 5).await;
    assert_eq!(wal_info(&node.client).await["writable"], true);
    let acknowledged = node.client.get_instance("c-1").await.unwrap();
    let segment = data
        .path()
        .join("wal")
        .join(rstmdb_wal::segment::segment_filename(1));
    let seq = node.engine.wal().next_sequence();

    std::fs::remove_dir_all(data.path().join("wal")).unwrap();

    // INFO shows the condition before any write is attempted.
    let wal = wal_info(&node.client).await;
    assert_eq!(wal["writable"], false);
    let error = wal["error"].as_str().unwrap();
    assert!(error.contains(&segment.display().to_string()), "{}", error);

    // Every write is refused, including other operations.
    assert_write_refused(&node.client, &segment, "deleted").await;
    assert_write_refused(&node.client, &segment, "deleted").await;
    let err = node
        .client
        .create_instance("counter", 1, Some("c-2"), None, None)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        ClientError::ServerError {
            code: ErrorCode::WalIoError,
            ..
        }
    ));
    assert_eq!(
        node.engine.wal().next_sequence(),
        seq,
        "nothing was written"
    );

    // Everything acknowledged before is still served, and the refused writes
    // changed nothing.
    let after = node.client.get_instance("c-1").await.unwrap();
    assert_eq!(after.ctx, acknowledged.ctx);
    assert_eq!(after.ctx["n"], 5);
    assert_eq!(after.last_wal_offset, acknowledged.last_wal_offset);
    assert!(node.client.get_instance("c-2").await.is_err());
    assert_eq!(wal_info(&node.client).await["writable"], false);
    stop(node).await;
}

#[cfg(unix)]
#[tokio::test]
async fn replacing_the_wal_directory_refuses_writes_and_keeps_acknowledged_data() {
    // What happened on the live server: the data directory was recreated
    // from a copy while the server kept writing to the old, deleted file.
    let data = TempDir::new().unwrap();
    let node = start(data.path(), 64 * 1024).await;
    seed(&node.client, 5).await;
    let acknowledged = node.client.get_instance("c-1").await.unwrap();

    let wal_dir = data.path().join("wal");
    let copy = data.path().join("wal-copy");
    std::fs::create_dir(&copy).unwrap();
    for entry in std::fs::read_dir(&wal_dir).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
    }
    std::fs::remove_dir_all(&wal_dir).unwrap();
    std::fs::rename(&copy, &wal_dir).unwrap();
    let segment = wal_dir.join(rstmdb_wal::segment::segment_filename(1));

    assert_write_refused(&node.client, &segment, "replaced").await;
    let wal = wal_info(&node.client).await;
    assert_eq!(wal["writable"], false);
    assert!(wal["error"]
        .as_str()
        .unwrap()
        .contains(&segment.display().to_string()));
    stop(node).await;

    // Nothing acknowledged is lost: the restarted server reads the copy,
    // which holds every acknowledged write, and accepts writes again.
    let node = start(data.path(), 64 * 1024).await;
    let restored = node.client.get_instance("c-1").await.unwrap();
    assert_eq!(restored.ctx, acknowledged.ctx);
    assert_eq!(restored.last_wal_offset, acknowledged.last_wal_offset);
    assert_eq!(wal_info(&node.client).await["writable"], true);
    node.client
        .apply_event("c-1", "TICK", Some(json!({"n": 6})), None, None)
        .await
        .unwrap();
    stop(node).await;
}

#[tokio::test]
async fn rotation_and_compaction_never_refuse_writes() {
    let data = TempDir::new().unwrap();
    // Small segments: dozens of rotations.
    let node = start(data.path(), 4096).await;
    seed(&node.client, 150).await;
    let segments = node.engine.wal().segment_ids();
    assert!(
        segments.len() >= 5,
        "expected rotations, got {:?}",
        segments
    );

    let compacted = node.client.compact(true).await.unwrap();
    assert!(compacted["segments_deleted"].as_u64().unwrap() >= 1);
    assert!(!node.engine.wal().segment_ids().contains(&segments[0]));

    for n in 151..=200 {
        node.client
            .apply_event("c-1", "TICK", Some(json!({"n": n})), None, None)
            .await
            .unwrap();
    }
    assert_eq!(wal_info(&node.client).await["writable"], true);
    assert_eq!(node.client.get_instance("c-1").await.unwrap().ctx["n"], 200);
    stop(node).await;
}

#[tokio::test]
async fn compaction_and_snapshot_refuse_and_preserve_files_when_detached() {
    let data = TempDir::new().unwrap();
    let node = start(data.path(), 4096).await;
    seed(&node.client, 150).await;
    let segments = node.engine.wal().segment_ids();
    assert!(
        segments.len() >= 4,
        "expected rotations, got {:?}",
        segments
    );

    let wal_dir = data.path().join("wal");
    #[cfg(unix)]
    {
        let copy = data.path().join("wal-copy");
        std::fs::create_dir(&copy).unwrap();
        for entry in std::fs::read_dir(&wal_dir).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), copy.join(entry.file_name())).unwrap();
        }
        std::fs::remove_dir_all(&wal_dir).unwrap();
        std::fs::rename(&copy, &wal_dir).unwrap();
    }
    #[cfg(not(unix))]
    {
        let current = wal_dir.join(rstmdb_wal::segment::segment_filename(
            *segments.last().unwrap(),
        ));
        std::fs::remove_file(&current).unwrap();
    }

    let snap_err = node.client.snapshot_instance("c-1").await.unwrap_err();
    assert!(matches!(
        snap_err,
        ClientError::ServerError {
            code: ErrorCode::WalIoError,
            retryable: false,
            ..
        }
    ));

    let compact_err = node.client.compact(true).await.unwrap_err();
    assert!(matches!(
        compact_err,
        ClientError::ServerError {
            code: ErrorCode::WalIoError,
            retryable: false,
            ..
        }
    ));

    // None of the older WAL segments on disk were deleted by the refused compaction.
    for &seg_id in &segments[..segments.len() - 1] {
        assert!(
            wal_dir
                .join(rstmdb_wal::segment::segment_filename(seg_id))
                .exists(),
            "segment {} should remain intact",
            seg_id
        );
    }
    assert_eq!(wal_info(&node.client).await["writable"], false);
    stop(node).await;
}
