//! End-to-end hot backup over the protocol: back up a running server, restore
//! the downloaded archive into a fresh data dir, and assert the state replays.
//!
//! This proves the full wire path (not just the in-handler unit test): a real
//! `rstmdb_server::Server` bound to a TCP socket, a real `rstmdb_client::Client`
//! talking to it, `BackupBegin`/`BackupChunk`/`BackupEnd` driven over that
//! connection, the assembled bytes verified with `rstmdb_backup::verify_backup`,
//! restored with `rstmdb_backup::read_backup` into a brand-new data dir, and a
//! freshly opened `StateMachineEngine` on that dir asserted to match the
//! original state.

use base64::Engine as _;
use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::message::Operation;
use rstmdb_wal::{FsyncPolicy, WalConfig};
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;

/// Starts a real server on a free TCP port with its own temp data dir.
///
/// Binds to port 0 to find a free port, then releases it before the server
/// re-binds the same address — there's a small window where another process
/// could steal the port, so callers must retry-connect (see `connect_client`).
async fn start_server() -> (Arc<rstmdb_server::Server>, std::net::SocketAddr, TempDir) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let data_dir = TempDir::new().unwrap();
    let engine = Arc::new(
        StateMachineEngine::new(
            WalConfig::new(data_dir.path().join("wal")).with_fsync_policy(FsyncPolicy::EveryWrite),
        )
        .unwrap(),
    );
    let server = Arc::new(rstmdb_server::Server::new(
        rstmdb_server::ServerConfig::new(addr),
        engine,
    ));
    {
        let srv = server.clone();
        tokio::spawn(async move {
            let _ = srv.run().await;
        });
    }
    (server, addr, data_dir)
}

/// Connects a client to `addr`, retrying while the server finishes binding,
/// then spawns the mandatory read loop and yields once so it starts polling.
async fn connect_client(addr: std::net::SocketAddr) -> rstmdb_client::Client {
    let client = rstmdb_client::Client::new(
        rstmdb_client::ConnectionConfig::new(addr).with_client_name("hot-backup-test"),
    );

    let mut last_err = None;
    let mut connected = false;
    for _ in 0..30 {
        match client.connect().await {
            Ok(()) => {
                connected = true;
                break;
            }
            Err(e) => {
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    assert!(connected, "failed to connect to server: {last_err:?}");

    let conn = client.connection();
    tokio::spawn({
        let conn = conn.clone();
        async move {
            let _ = conn.read_loop().await;
        }
    });
    tokio::task::yield_now().await;

    client
}

#[tokio::test]
async fn hot_backup_roundtrip_over_protocol() {
    // 1. Start a real server on a real socket, connect a real client.
    let (_server, addr, _src_data_dir) = start_server().await;
    let client = connect_client(addr).await;

    // 2. Write data through the client (over the wire, not straight to the engine).
    client
        .put_machine(
            "order",
            1,
            json!({
                "states": ["created", "paid"],
                "initial": "created",
                "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
            }),
        )
        .await
        .unwrap();
    for i in 0..10 {
        client
            .create_instance(
                "order",
                1,
                Some(&format!("o-{i}")),
                Some(json!({"n": i})),
                None,
            )
            .await
            .unwrap();
    }
    client
        .apply_event("o-0", "PAY", Some(json!({})), None, None)
        .await
        .unwrap();

    // 3. Hot backup over the protocol: BEGIN, drain CHUNK, END.
    let conn = client.connection();
    let begin = conn
        .request(Operation::BackupBegin, json!({"compression": "gzip"}))
        .await
        .unwrap();
    assert!(begin.is_ok(), "backup begin failed: {:?}", begin.error);
    let r = begin.result.unwrap();
    let backup_id = r["backup_id"]
        .as_str()
        .expect("backup_id missing in BackupBegin response")
        .to_string();
    let total = r["total_bytes"]
        .as_u64()
        .expect("total_bytes missing in BackupBegin response");
    assert!(total > 0, "expected a non-empty archive");

    let mut archive = Vec::new();
    let mut offset = 0u64;
    loop {
        let resp = conn
            .request(
                Operation::BackupChunk,
                json!({"backup_id": backup_id, "offset": offset, "len": 4u64 * 1024 * 1024}),
            )
            .await
            .unwrap();
        assert!(resp.is_ok(), "backup chunk failed: {:?}", resp.error);
        let body = resp.result.unwrap();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(body["bytes"].as_str().unwrap())
            .unwrap();
        offset += bytes.len() as u64;
        archive.extend_from_slice(&bytes);
        if body["eof"].as_bool().unwrap() {
            break;
        }
    }
    assert_eq!(
        offset, total,
        "downloaded byte count must match total_bytes"
    );

    let end = conn
        .request(Operation::BackupEnd, json!({"backup_id": backup_id}))
        .await
        .unwrap();
    assert!(end.is_ok(), "backup end failed: {:?}", end.error);

    // 4. Verify the assembled bytes (checksums + manifest), then restore into
    //    a fresh, unrelated data dir — proving these are real, self-contained
    //    archive bytes and not just a passthrough of the source dir.
    rstmdb_backup::verify_backup(std::io::Cursor::new(archive.clone())).unwrap();

    let dst = TempDir::new().unwrap();
    rstmdb_backup::read_backup(std::io::Cursor::new(archive), dst.path(), false).unwrap();

    // 5. Reopen a brand-new engine on the restored dir and assert parity with
    //    what was written through the live server.
    let restored = StateMachineEngine::new(
        WalConfig::new(dst.path().join("wal")).with_fsync_policy(FsyncPolicy::EveryWrite),
    )
    .unwrap();
    assert_eq!(restored.get_all_instances().len(), 10);
    assert_eq!(restored.get_instance("o-0").unwrap().state, "paid");
    assert_eq!(restored.get_instance("o-5").unwrap().ctx["n"], 5);
}
