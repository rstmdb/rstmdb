//! End-to-end: write real data through the engine → back it up → restore into a
//! fresh data dir → reopen the engine → assert the state replays identically.

use rstmdb_backup::{read_backup, verify_backup, write_backup, Compression, ManifestMeta};
use rstmdb_core::StateMachineEngine;
use rstmdb_wal::{FsyncPolicy, WalConfig};
use serde_json::json;
use std::path::Path;

fn open_engine(data_dir: &Path) -> StateMachineEngine {
    StateMachineEngine::new(
        WalConfig::new(data_dir.join("wal")).with_fsync_policy(FsyncPolicy::EveryWrite),
    )
    .unwrap()
}

#[test]
fn backup_restore_roundtrip_via_engine() {
    let src = tempfile::TempDir::new().unwrap();

    // Populate the source data dir through the engine, then release the WAL.
    {
        let e = open_engine(src.path());
        e.put_machine(
            "order",
            1,
            &json!({
                "states": ["created", "paid"],
                "initial": "created",
                "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
            }),
        )
        .unwrap();
        for i in 0..30 {
            e.create_instance(&format!("o-{i}"), "order", 1, json!({ "n": i }), None)
                .unwrap();
        }
        e.apply_event("o-0", "PAY", json!({}), None, None, None, None)
            .unwrap();
        e.wal().sync().unwrap();
    }

    // Back up (gzip) to an in-memory archive.
    let mut archive = Vec::new();
    let m = write_backup(
        src.path(),
        ManifestMeta {
            rstmdb_version: "test".into(),
            instance_count: Some(30),
            ..Default::default()
        },
        Compression::Gzip,
        &mut archive,
    )
    .unwrap();
    assert!(m.segment_count >= 1);

    // Verify without extracting.
    verify_backup(std::io::Cursor::new(archive.clone())).unwrap();

    // Restore into a fresh data dir.
    let dst = tempfile::TempDir::new().unwrap();
    read_backup(std::io::Cursor::new(archive), dst.path(), false).unwrap();

    // Reopen the engine on the restored dir → WAL replay → assert parity.
    let e2 = open_engine(dst.path());
    assert!(e2.get_machine("order", 1).is_ok(), "machine restored");
    assert_eq!(e2.get_all_instances().len(), 30, "all instances restored");
    assert_eq!(e2.get_instance("o-0").unwrap().state, "paid", "applied event restored");
    let mid = e2.get_instance("o-15").unwrap();
    assert_eq!(mid.state, "created");
    assert_eq!(mid.ctx["n"], 15, "context restored");
}
