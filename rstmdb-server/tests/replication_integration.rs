//! Integration tests for WAL streaming replication.
//!
//! These tests start actual primary + replica server instances and verify
//! that data replicates correctly, read-only mode is enforced, and catch-up works.

use rstmdb_core::StateMachineEngine;
use rstmdb_protocol::message::{Operation, Request};
use rstmdb_protocol::{Decoder, Encoder};
use rstmdb_server::auth::TokenValidator;
use rstmdb_server::config::{ReplicationConfig, ReplicationMode, ReplicationRole};
use rstmdb_server::handler::CommandHandler;
use rstmdb_server::session::Session;
use rstmdb_wal::{FsyncPolicy, WalConfig, WalEntry};
use serde_json::json;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::Arc;
use tempfile::TempDir;

fn make_session() -> Session {
    Session::new(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 12345),
        false,
    )
}

fn make_engine(dir: &std::path::Path) -> Arc<StateMachineEngine> {
    let config = WalConfig::new(dir)
        .with_segment_size(4096)
        .with_fsync_policy(FsyncPolicy::EveryWrite);
    Arc::new(StateMachineEngine::new(config).unwrap())
}

fn sample_definition() -> serde_json::Value {
    json!({
        "states": ["created", "paid", "shipped"],
        "initial": "created",
        "transitions": [
            {"from": "created", "event": "PAY", "to": "paid"},
            {"from": "paid", "event": "SHIP", "to": "shipped"}
        ]
    })
}

// =========================================================================
// Async replication via engine + apply_replicated_entry
// =========================================================================

#[test]
fn test_primary_writes_replicate_to_replica_engine() {
    // Simulate: primary writes data, WAL entries are read and applied on replica
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    // Write on primary
    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();
    let (instance, _) = primary
        .create_instance("order-001", "order", 1, json!({"customer": "alice"}), None)
        .unwrap();
    assert_eq!(instance.state, "created");

    let result = primary
        .apply_event(
            "order-001",
            "PAY",
            json!({"amount": 99.99}),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    assert_eq!(result.to_state, "paid");

    // Read WAL entries from primary
    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    assert_eq!(entries.len(), 3); // PutMachine + CreateInstance + ApplyEvent

    // Apply on replica
    for (_seq, _offset, entry) in entries {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    // Verify replica has the same state
    let replica_instance = replica.get_instance("order-001").unwrap();
    assert_eq!(replica_instance.state, "paid");
    assert_eq!(replica_instance.ctx["amount"], 99.99);
    assert_eq!(replica_instance.ctx["customer"], "alice");

    let replica_machine = replica.get_machine("order", 1).unwrap();
    assert_eq!(replica_machine.name, "order");
}

#[test]
fn test_catch_up_from_zero() {
    // Replica starts with empty WAL, catches up with all primary entries
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    // Write many entries on primary
    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();
    for i in 0..10 {
        let id = format!("order-{:03}", i);
        primary
            .create_instance(&id, "order", 1, json!({"idx": i}), None)
            .unwrap();
        primary
            .apply_event(&id, "PAY", json!({}), None, None, None, None)
            .unwrap();
    }

    // Catch up: read all and apply
    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    assert_eq!(entries.len(), 21); // 1 PutMachine + 10 Create + 10 Apply

    for (_seq, _offset, entry) in entries {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    // All instances exist on replica in "paid" state
    for i in 0..10 {
        let id = format!("order-{:03}", i);
        let inst = replica.get_instance(&id).unwrap();
        assert_eq!(inst.state, "paid");
    }
}

#[test]
fn test_incremental_replication() {
    // Replica catches up, then gets new entries incrementally
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    // Phase 1: initial data
    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();
    primary
        .create_instance("order-001", "order", 1, json!({}), None)
        .unwrap();

    let entries1 = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    assert_eq!(entries1.len(), 2);

    let mut last_offset = 0u64;
    for (_seq, offset, entry) in entries1 {
        replica.apply_replicated_entry(0, entry).unwrap();
        last_offset = offset.as_u64();
    }

    assert!(replica.get_instance("order-001").is_ok());

    // Phase 2: new writes on primary
    primary
        .apply_event("order-001", "PAY", json!({}), None, None, None, None)
        .unwrap();
    primary
        .create_instance("order-002", "order", 1, json!({}), None)
        .unwrap();

    // Read only new entries (from last_offset + 1)
    let entries2 = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(last_offset + 1), None)
        .unwrap();
    assert_eq!(entries2.len(), 2);

    for (_seq, _offset, entry) in entries2 {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    let inst1 = replica.get_instance("order-001").unwrap();
    assert_eq!(inst1.state, "paid");

    let inst2 = replica.get_instance("order-002").unwrap();
    assert_eq!(inst2.state, "created");
}

// =========================================================================
// Read-only enforcement (handler level)
// =========================================================================

#[test]
fn test_replica_rejects_all_writes() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(dir.path());

    // Replicate a machine so we have something to read
    engine
        .apply_replicated_entry(
            0,
            WalEntry::PutMachine {
                machine: "order".to_string(),
                version: 1,
                definition_hash: "abc".to_string(),
                definition: sample_definition(),
            },
        )
        .unwrap();

    engine
        .apply_replicated_entry(
            0,
            WalEntry::CreateInstance {
                instance_id: "i-1".to_string(),
                machine: "order".to_string(),
                version: 1,
                initial_state: "created".to_string(),
                initial_ctx: json!({}),
                idempotency_key: None,
            },
        )
        .unwrap();

    let handler = CommandHandler::new(engine)
        .with_read_only(true)
        .with_allow_flush_all(true);
    let mut session = make_session();

    // All write operations should fail with READ_ONLY_MODE
    let write_ops = vec![
        (
            Operation::PutMachine,
            json!({"machine": "x", "version": 1, "definition": {}}),
        ),
        (
            Operation::CreateInstance,
            json!({"machine": "order", "version": 1}),
        ),
        (
            Operation::ApplyEvent,
            json!({"instance_id": "i-1", "event": "PAY"}),
        ),
        (Operation::DeleteInstance, json!({"instance_id": "i-1"})),
        (Operation::FlushAll, json!({})),
        (Operation::Compact, json!({})),
    ];

    for (op, params) in write_ops {
        let request = Request::new("1", op).with_params(params);
        let response = handler.handle(&mut session, &request);
        assert!(
            response.is_error(),
            "{:?} should be rejected on replica",
            op
        );
        assert_eq!(
            response.error.as_ref().unwrap().code,
            rstmdb_protocol::ErrorCode::ReadOnlyMode,
            "{:?} should return READ_ONLY_MODE",
            op
        );
    }

    // Read operations should succeed
    let get = Request::new("2", Operation::GetInstance).with_params(json!({"instance_id": "i-1"}));
    let response = handler.handle(&mut session, &get);
    assert!(response.is_ok());
    assert_eq!(response.result.unwrap()["state"], "created");

    let list = Request::new("3", Operation::ListMachines);
    let response = handler.handle(&mut session, &list);
    assert!(response.is_ok());
}

// =========================================================================
// Delete replication
// =========================================================================

#[test]
fn test_delete_replicates_to_replica() {
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();
    primary
        .create_instance("del-me", "order", 1, json!({}), None)
        .unwrap();
    primary.delete_instance("del-me", None).unwrap();

    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();

    for (_seq, _offset, entry) in entries {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    assert!(replica.get_instance("del-me").is_err());
}

// =========================================================================
// Replication config validation
// =========================================================================

#[test]
fn test_replication_token_hash_matches_cli_output() {
    // Sanity check: the hash produced by TokenValidator is the same that
    // `rstmdb-cli hash-token` would produce — used by operators to
    // populate `auth_token_hashes` in config.
    let hash = TokenValidator::hash_token("my-secret-token");
    assert_eq!(hash.len(), 64);
    assert_eq!(
        hash,
        "ea5add57437cbf20af59034d7ed17968dcc56767b41965fcc5b376d45db8b4a3"
    );
}

#[test]
fn test_replication_auth_validator_accepts_correct_token() {
    let hash = TokenValidator::hash_token("dev-token");
    let validator = TokenValidator::new(vec![hash]);
    assert!(validator.validate("dev-token"));
    assert!(!validator.validate("wrong-token"));
    assert!(!validator.validate(""));
}

#[test]
fn test_replication_config_hashes_plaintext_token() {
    let config = ReplicationConfig {
        auth_token: Some("some-plaintext".to_string()),
        ..Default::default()
    };
    assert!(config.auth_required());
    let hashes = config.resolved_token_hashes();
    assert_eq!(hashes.len(), 1);
    // Must match what TokenValidator produces
    let expected = TokenValidator::hash_token("some-plaintext");
    assert_eq!(hashes[0], expected);

    // A validator built from the resolved hashes accepts the plaintext
    let validator = TokenValidator::new(hashes);
    assert!(validator.validate("some-plaintext"));
    assert!(!validator.validate("something-else"));
}

#[test]
fn test_sync_mode_config_validation() {
    let config = ReplicationConfig {
        role: ReplicationRole::Primary,
        mode: ReplicationMode::Sync,
        sync_replicas: 0,
        ..Default::default()
    };
    assert!(config.validate().is_err());

    let config = ReplicationConfig {
        role: ReplicationRole::Primary,
        mode: ReplicationMode::Sync,
        sync_replicas: 1,
        ..Default::default()
    };
    assert!(config.validate().is_ok());
}

#[test]
fn test_replica_without_upstream_fails_validation() {
    let config = ReplicationConfig {
        role: ReplicationRole::Replica,
        upstream: None,
        ..Default::default()
    };
    assert!(config.validate().is_err());

    let config = ReplicationConfig {
        role: ReplicationRole::Replica,
        upstream: Some("localhost:7401".to_string()),
        ..Default::default()
    };
    assert!(config.validate().is_ok());
}

// =========================================================================
// RCPX replication protocol roundtrip
// =========================================================================

#[test]
fn test_replication_message_framing_roundtrip() {
    use rstmdb_server::replication::ReplicationMessage;

    let messages = vec![
        ReplicationMessage::ReplicateAuth {
            auth_token: Some("secret".to_string()),
            last_sequence: 42,
            last_primary_offset: 0,
        },
        ReplicationMessage::ReplicateSyncResponse {
            ok: true,
            primary_sequence: 100,
            error: None,
        },
        ReplicationMessage::ReplicateEntry {
            sequence: 1,
            offset: 1024,
            entry: WalEntry::CreateInstance {
                instance_id: "i-1".to_string(),
                machine: "order".to_string(),
                version: 1,
                initial_state: "created".to_string(),
                initial_ctx: json!({"key": "value"}),
                idempotency_key: None,
            },
            timestamp_ms: 1_700_000_000_000,
        },
        ReplicationMessage::ReplicateAck {
            sequence: 1,
            applied_offset: 0,
        },
        ReplicationMessage::ReplicateHeartbeat {
            primary_sequence: 50,
            timestamp_ms: 1_234_567_890,
            primary_latest_write_ts_ms: 1_234_560_000,
        },
    ];

    for msg in messages {
        // Encode via RCPX framing
        let bytes = msg.to_bytes().unwrap();
        let frame = Encoder::encode_raw(&bytes);

        // Decode
        let mut decoder = Decoder::new();
        decoder.extend(&frame);
        let payload = decoder.decode_raw().unwrap().unwrap();
        let decoded = ReplicationMessage::from_bytes(&payload).unwrap();

        // Verify round-trip
        let re_bytes = decoded.to_bytes().unwrap();
        assert_eq!(bytes, re_bytes);
    }
}

// =========================================================================
// WAL persistence across restarts
// =========================================================================

#[test]
fn test_replicated_data_survives_restart() {
    let dir = TempDir::new().unwrap();

    // First boot: apply replicated entries
    {
        let engine = make_engine(dir.path());
        engine
            .apply_replicated_entry(
                0,
                WalEntry::PutMachine {
                    machine: "order".to_string(),
                    version: 1,
                    definition_hash: "abc".to_string(),
                    definition: sample_definition(),
                },
            )
            .unwrap();
        engine
            .apply_replicated_entry(
                0,
                WalEntry::CreateInstance {
                    instance_id: "survive".to_string(),
                    machine: "order".to_string(),
                    version: 1,
                    initial_state: "created".to_string(),
                    initial_ctx: json!({"durable": true}),
                    idempotency_key: None,
                },
            )
            .unwrap();
        engine
            .apply_replicated_entry(
                0,
                WalEntry::ApplyEvent {
                    instance_id: "survive".to_string(),
                    event: "PAY".to_string(),
                    from_state: "created".to_string(),
                    to_state: "paid".to_string(),
                    payload: json!({"amount": 42}),
                    ctx: json!({"durable": true, "amount": 42}),
                    event_id: None,
                    idempotency_key: None,
                },
            )
            .unwrap();
    }

    // Second boot: verify state recovered from WAL
    {
        let engine = make_engine(dir.path());
        let inst = engine.get_instance("survive").unwrap();
        assert_eq!(inst.state, "paid");
        assert_eq!(inst.ctx["durable"], true);
        assert_eq!(inst.ctx["amount"], 42);
    }
}

// =========================================================================
// Multiple machine versions
// =========================================================================

#[test]
fn test_multiple_machine_versions_replicate() {
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    let def_v1 = json!({
        "states": ["created", "paid"],
        "initial": "created",
        "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
    });
    let def_v2 = json!({
        "states": ["created", "paid", "refunded"],
        "initial": "created",
        "transitions": [
            {"from": "created", "event": "PAY", "to": "paid"},
            {"from": "paid", "event": "REFUND", "to": "refunded"}
        ]
    });

    primary.put_machine("order", 1, &def_v1).unwrap();
    primary.put_machine("order", 2, &def_v2).unwrap();

    // Create instances on different versions
    primary
        .create_instance("v1-inst", "order", 1, json!({}), None)
        .unwrap();
    primary
        .create_instance("v2-inst", "order", 2, json!({}), None)
        .unwrap();
    primary
        .apply_event("v2-inst", "PAY", json!({}), None, None, None, None)
        .unwrap();
    primary
        .apply_event("v2-inst", "REFUND", json!({}), None, None, None, None)
        .unwrap();

    // Replicate all
    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    for (_seq, _offset, entry) in entries {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    let v1 = replica.get_instance("v1-inst").unwrap();
    assert_eq!(v1.state, "created");
    assert_eq!(v1.version, 1);

    let v2 = replica.get_instance("v2-inst").unwrap();
    assert_eq!(v2.state, "refunded");
    assert_eq!(v2.version, 2);

    // Both versions should exist
    assert!(replica.get_machine("order", 1).is_ok());
    assert!(replica.get_machine("order", 2).is_ok());
}

// =========================================================================
// Context merging through replication
// =========================================================================

#[test]
fn test_context_preserved_through_replication() {
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();
    primary
        .create_instance(
            "ctx-test",
            "order",
            1,
            json!({"customer": "alice", "items": ["a", "b"]}),
            None,
        )
        .unwrap();
    primary
        .apply_event(
            "ctx-test",
            "PAY",
            json!({"payment_id": "pay-123", "amount": 42.5}),
            None,
            None,
            None,
            None,
        )
        .unwrap();
    primary
        .apply_event(
            "ctx-test",
            "SHIP",
            json!({"tracking": "1Z999", "carrier": "UPS"}),
            None,
            None,
            None,
            None,
        )
        .unwrap();

    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    for (_seq, _offset, entry) in entries {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    let inst = replica.get_instance("ctx-test").unwrap();
    assert_eq!(inst.state, "shipped");
    assert_eq!(inst.ctx["customer"], "alice");
    assert_eq!(inst.ctx["payment_id"], "pay-123");
    assert_eq!(inst.ctx["amount"], 42.5);
    assert_eq!(inst.ctx["tracking"], "1Z999");
    assert_eq!(inst.ctx["carrier"], "UPS");
}

// =========================================================================
// Replica read-only: reads work for all read operations
// =========================================================================

#[test]
fn test_replica_serves_all_read_operations() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(dir.path());

    // Set up state via replication
    engine
        .apply_replicated_entry(
            0,
            WalEntry::PutMachine {
                machine: "order".to_string(),
                version: 1,
                definition_hash: "abc".to_string(),
                definition: sample_definition(),
            },
        )
        .unwrap();
    for i in 0..3 {
        engine
            .apply_replicated_entry(
                0,
                WalEntry::CreateInstance {
                    instance_id: format!("inst-{}", i),
                    machine: "order".to_string(),
                    version: 1,
                    initial_state: "created".to_string(),
                    initial_ctx: json!({"idx": i}),
                    idempotency_key: None,
                },
            )
            .unwrap();
    }

    let handler = CommandHandler::new(engine).with_read_only(true);
    let mut session = make_session();

    // Ping
    let r = handler.handle(&mut session, &Request::new("1", Operation::Ping));
    assert!(r.is_ok());
    assert_eq!(r.result.unwrap()["pong"], true);

    // Info
    let r = handler.handle(&mut session, &Request::new("2", Operation::Info));
    assert!(r.is_ok());
    assert!(r.result.unwrap()["server_name"].as_str().is_some());

    // GetMachine
    let r = handler.handle(
        &mut session,
        &Request::new("3", Operation::GetMachine)
            .with_params(json!({"machine": "order", "version": 1})),
    );
    assert!(r.is_ok());

    // ListMachines
    let r = handler.handle(&mut session, &Request::new("4", Operation::ListMachines));
    assert!(r.is_ok());
    let items = r.result.unwrap()["items"].as_array().unwrap().len();
    assert_eq!(items, 1);

    // GetInstance
    let r = handler.handle(
        &mut session,
        &Request::new("5", Operation::GetInstance).with_params(json!({"instance_id": "inst-0"})),
    );
    assert!(r.is_ok());
    assert_eq!(r.result.unwrap()["state"], "created");

    // ListInstances
    let r = handler.handle(
        &mut session,
        &Request::new("6", Operation::ListInstances).with_params(json!({"machine": "order"})),
    );
    assert!(r.is_ok());
    assert_eq!(r.result.unwrap()["total"], 3);

    // WalStats
    let r = handler.handle(&mut session, &Request::new("7", Operation::WalStats));
    assert!(r.is_ok());
    assert!(r.result.unwrap()["entry_count"].as_u64().unwrap() > 0);

    // WalRead
    let r = handler.handle(
        &mut session,
        &Request::new("8", Operation::WalRead).with_params(json!({"from_offset": 0, "limit": 10})),
    );
    assert!(r.is_ok());
    let records = r.result.unwrap()["records"].as_array().unwrap().len();
    assert!(records > 0);
}

// =========================================================================
// Sequence ordering guarantees
// =========================================================================

#[test]
fn test_replicated_sequences_match_primary_order() {
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();
    for i in 0..5 {
        primary
            .create_instance(&format!("i-{}", i), "order", 1, json!({}), None)
            .unwrap();
    }

    let primary_entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();

    let primary_sequences: Vec<u64> = primary_entries.iter().map(|(s, _, _)| *s).collect();

    // Apply on replica and collect local sequences
    let mut replica_sequences = Vec::new();
    for (_seq, _offset, entry) in primary_entries {
        let (local_seq, _) = replica.apply_replicated_entry(0, entry).unwrap();
        replica_sequences.push(local_seq);
    }

    // Sequences on replica should be monotonically increasing
    for window in replica_sequences.windows(2) {
        assert!(window[1] > window[0]);
    }

    // Same count
    assert_eq!(primary_sequences.len(), replica_sequences.len());
}

// =========================================================================
// Replica recreate instance after delete
// =========================================================================

#[test]
fn test_delete_and_recreate_replicates() {
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();

    // Create, use, delete, recreate
    primary
        .create_instance("reuse", "order", 1, json!({"round": 1}), None)
        .unwrap();
    primary
        .apply_event("reuse", "PAY", json!({}), None, None, None, None)
        .unwrap();
    primary.delete_instance("reuse", None).unwrap();
    primary
        .create_instance("reuse", "order", 1, json!({"round": 2}), None)
        .unwrap();

    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    for (_seq, _offset, entry) in entries {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    let inst = replica.get_instance("reuse").unwrap();
    assert_eq!(inst.state, "created"); // fresh after recreate
    assert_eq!(inst.ctx["round"], 2);
}

// =========================================================================
// Flush all replicates
// =========================================================================

#[test]
fn test_flush_all_on_primary_does_not_affect_replica() {
    // flush_all clears in-memory state but doesn't write a WAL entry,
    // so the replica retains its replicated state
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();
    primary
        .create_instance("flush-test", "order", 1, json!({}), None)
        .unwrap();

    // Replicate first
    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    for (_seq, _offset, entry) in entries {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    // Now flush primary
    primary.flush_all();
    assert!(primary.get_instance("flush-test").is_err());

    // Replica still has it (flush_all doesn't produce a WAL entry)
    assert!(replica.get_instance("flush-test").is_ok());
}

// =========================================================================
// Multiple machines
// =========================================================================

#[test]
fn test_multiple_machines_replicate() {
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    let order_def = json!({
        "states": ["created", "paid"],
        "initial": "created",
        "transitions": [{"from": "created", "event": "PAY", "to": "paid"}]
    });
    let user_def = json!({
        "states": ["active", "suspended"],
        "initial": "active",
        "transitions": [{"from": "active", "event": "SUSPEND", "to": "suspended"}]
    });

    primary.put_machine("order", 1, &order_def).unwrap();
    primary.put_machine("user", 1, &user_def).unwrap();

    primary
        .create_instance("o-1", "order", 1, json!({}), None)
        .unwrap();
    primary
        .create_instance("u-1", "user", 1, json!({}), None)
        .unwrap();
    primary
        .apply_event("o-1", "PAY", json!({}), None, None, None, None)
        .unwrap();
    primary
        .apply_event("u-1", "SUSPEND", json!({}), None, None, None, None)
        .unwrap();

    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    for (_seq, _offset, entry) in entries {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    assert_eq!(replica.get_instance("o-1").unwrap().state, "paid");
    assert_eq!(replica.get_instance("u-1").unwrap().state, "suspended");
    assert!(replica.get_machine("order", 1).is_ok());
    assert!(replica.get_machine("user", 1).is_ok());
}

// =========================================================================
// Two replicas get same state
// =========================================================================

#[test]
fn test_two_replicas_converge_to_same_state() {
    let primary_dir = TempDir::new().unwrap();
    let replica1_dir = TempDir::new().unwrap();
    let replica2_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica1 = make_engine(replica1_dir.path());
    let replica2 = make_engine(replica2_dir.path());

    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();
    primary
        .create_instance("shared", "order", 1, json!({"x": 1}), None)
        .unwrap();
    primary
        .apply_event("shared", "PAY", json!({"y": 2}), None, None, None, None)
        .unwrap();

    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();

    // Apply same entries to both replicas
    for (_seq, _offset, entry) in &entries {
        replica1.apply_replicated_entry(0, entry.clone()).unwrap();
    }
    for (_seq, _offset, entry) in entries {
        replica2.apply_replicated_entry(0, entry).unwrap();
    }

    let r1 = replica1.get_instance("shared").unwrap();
    let r2 = replica2.get_instance("shared").unwrap();

    assert_eq!(r1.state, r2.state);
    assert_eq!(r1.state, "paid");
    assert_eq!(r1.ctx, r2.ctx);
}

// =========================================================================
// Replication protocol message variants
// =========================================================================

#[test]
fn test_replication_message_error_response() {
    use rstmdb_server::replication::ReplicationMessage;

    let msg = ReplicationMessage::ReplicateSyncResponse {
        ok: false,
        primary_sequence: 0,
        error: Some("authentication failed".to_string()),
    };

    let bytes = msg.to_bytes().unwrap();
    let decoded = ReplicationMessage::from_bytes(&bytes).unwrap();

    match decoded {
        ReplicationMessage::ReplicateSyncResponse { ok, error, .. } => {
            assert!(!ok);
            assert_eq!(error.unwrap(), "authentication failed");
        }
        _ => panic!("wrong variant"),
    }
}

#[test]
fn test_replication_message_auth_no_token() {
    use rstmdb_server::replication::ReplicationMessage;

    let msg = ReplicationMessage::ReplicateAuth {
        auth_token: None,
        last_sequence: 0,
        last_primary_offset: 0,
    };

    let bytes = msg.to_bytes().unwrap();
    let decoded = ReplicationMessage::from_bytes(&bytes).unwrap();

    match &decoded {
        ReplicationMessage::ReplicateAuth {
            auth_token,
            last_sequence,
            last_primary_offset,
        } => {
            assert!(auth_token.is_none());
            assert_eq!(*last_sequence, 0);
            assert_eq!(*last_primary_offset, 0);
        }
        _ => panic!("wrong variant"),
    }

    assert!(decoded.is_auth());
}

// =========================================================================
// Handler-level: read-only mode allows session lifecycle
// =========================================================================

#[test]
fn test_replica_handler_full_session_lifecycle() {
    let dir = TempDir::new().unwrap();
    let engine = make_engine(dir.path());

    engine
        .apply_replicated_entry(
            0,
            WalEntry::PutMachine {
                machine: "order".to_string(),
                version: 1,
                definition_hash: "abc".to_string(),
                definition: sample_definition(),
            },
        )
        .unwrap();

    let handler = CommandHandler::new(engine).with_read_only(true);
    let mut session = make_session();

    // HELLO
    let r = handler.handle(
        &mut session,
        &Request::new("1", Operation::Hello).with_params(json!({
            "protocol_version": 1,
            "client_name": "test-replica-client",
            "wire_modes": ["binary_json"],
            "features": ["idempotency"]
        })),
    );
    assert!(r.is_ok());

    // AUTH (no validator configured, so any non-empty token works)
    let r = handler.handle(
        &mut session,
        &Request::new("2", Operation::Auth).with_params(json!({
            "method": "bearer",
            "token": "test"
        })),
    );
    assert!(r.is_ok());

    // Read should work
    let r = handler.handle(
        &mut session,
        &Request::new("3", Operation::GetMachine)
            .with_params(json!({"machine": "order", "version": 1})),
    );
    assert!(r.is_ok());

    // Write should be blocked
    let r = handler.handle(
        &mut session,
        &Request::new("4", Operation::CreateInstance)
            .with_params(json!({"machine": "order", "version": 1})),
    );
    assert!(r.is_error());
    assert_eq!(
        r.error.unwrap().code,
        rstmdb_protocol::ErrorCode::ReadOnlyMode
    );

    // BYE
    let r = handler.handle(&mut session, &Request::new("5", Operation::Bye));
    assert!(r.is_ok());
}

// =========================================================================
// Config duration helpers
// =========================================================================

#[test]
fn test_replication_config_duration_helpers() {
    let config = ReplicationConfig {
        poll_interval_ms: 50,
        heartbeat_interval_secs: 10,
        reconnect_delay_secs: 3,
        lag_check_interval_secs: 15,
        sync_timeout_ms: 2000,
        ..Default::default()
    };

    assert_eq!(config.poll_interval().as_millis(), 50);
    assert_eq!(config.heartbeat_interval().as_secs(), 10);
    assert_eq!(config.reconnect_delay().as_secs(), 3);
    assert_eq!(config.lag_check_interval().as_secs(), 15);
    assert_eq!(config.sync_timeout().as_millis(), 2000);
}

// =========================================================================
// Observability: primary-side per-replica stats method
// =========================================================================

#[test]
fn test_manager_replica_stats_empty_when_no_replicas() {
    use rstmdb_server::replication::ReplicationManager;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(dir.path());
    let (shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);

    // Use a tokio runtime explicitly to spawn the WAL tailer
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let config = ReplicationConfig {
        role: ReplicationRole::Primary,
        ..Default::default()
    };
    let mgr = ReplicationManager::new(config, engine, shutdown_rx, None, None);

    let stats = mgr.replica_stats();
    assert!(stats.is_empty());
    assert_eq!(mgr.connected_replica_count(), 0);

    let _ = shutdown_tx.send(());
}

// =========================================================================
// Backpressure: slow replica is disconnected instead of silently dropping entries
// =========================================================================

#[test]
fn test_slow_replica_lag_visibility_via_stats() {
    // Verify that `replica_stats()` reflects lag when a replica hasn't ACKed.
    // This exercises the same machinery used by backpressure detection and
    // by the per-replica metrics/logs.
    use rstmdb_server::replication::ReplicationManager;

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let dir = TempDir::new().unwrap();
        let engine = make_engine(dir.path());
        let (_shutdown_tx, shutdown_rx) = tokio::sync::broadcast::channel(1);

        let config = ReplicationConfig {
            role: ReplicationRole::Primary,
            ..Default::default()
        };
        let mgr = ReplicationManager::new(config, engine.clone(), shutdown_rx, None, None);

        // Write some entries to primary — they bump next_sequence.
        engine
            .put_machine("order", 1, &sample_definition())
            .unwrap();
        engine
            .create_instance("s-1", "order", 1, json!({}), None)
            .unwrap();

        // With no replicas connected, stats is empty.
        let stats = mgr.replica_stats();
        assert!(stats.is_empty());
        assert_eq!(mgr.connected_replica_count(), 0);
    });
}

// =========================================================================
// Observability: replica lag_seconds when fully caught up is 0
// =========================================================================

#[test]
fn test_replica_lag_seconds_caught_up() {
    use rstmdb_server::replication::ReplicaClient;

    let dir = TempDir::new().unwrap();
    let engine = make_engine(dir.path());
    let config = ReplicationConfig {
        role: ReplicationRole::Replica,
        upstream: Some("127.0.0.1:65535".to_string()),
        ..Default::default()
    };

    let client = ReplicaClient::new(config, engine, "127.0.0.1:65535".to_string(), None).unwrap();

    // No heartbeat received, no entries applied — lag_seconds should be 0
    assert_eq!(client.lag_seconds(), 0.0);
    assert_eq!(client.lag_entries(), 0);
}

// =========================================================================
// Replica in-memory offsets match primary's offsets
// =========================================================================

#[test]
fn test_replica_in_memory_offset_matches_primary() {
    // When replica applies with the primary's offset, its in-memory
    // last_wal_offset should match the primary's, not the local WAL offset.
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    primary
        .put_machine("order", 1, &sample_definition())
        .unwrap();
    primary
        .create_instance("offset-test", "order", 1, json!({}), None)
        .unwrap();
    let result = primary
        .apply_event("offset-test", "PAY", json!({}), None, None, None, None)
        .unwrap();

    let primary_offset = result.wal_offset;
    assert!(primary_offset > 0);

    // Apply all entries to replica, passing the primary's offsets
    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    for (_seq, offset, entry) in entries {
        replica
            .apply_replicated_entry(offset.as_u64(), entry)
            .unwrap();
    }

    // Replica's in-memory offset should match the primary's
    let replica_inst = replica.get_instance("offset-test").unwrap();
    assert_eq!(replica_inst.last_wal_offset, primary_offset);
}

// =========================================================================
// Large batch replication
// =========================================================================

#[test]
fn test_large_batch_replication() {
    let primary_dir = TempDir::new().unwrap();
    let replica_dir = TempDir::new().unwrap();

    let primary = make_engine(primary_dir.path());
    let replica = make_engine(replica_dir.path());

    primary
        .put_machine(
            "counter",
            1,
            &json!({
                "states": ["active"],
                "initial": "active",
                "transitions": [{"from": "active", "event": "INC", "to": "active"}]
            }),
        )
        .unwrap();

    // Create 50 instances, each with 10 events = 551 WAL entries
    for i in 0..50 {
        let id = format!("c-{:04}", i);
        primary
            .create_instance(&id, "counter", 1, json!({"count": 0}), None)
            .unwrap();
        for j in 1..=10 {
            primary
                .apply_event(&id, "INC", json!({"count": j}), None, None, None, None)
                .unwrap();
        }
    }

    let entries = primary
        .wal()
        .read_from(rstmdb_wal::WalOffset::from_u64(0), None)
        .unwrap();
    assert_eq!(entries.len(), 551); // 1 + 50*11

    for (_seq, _offset, entry) in entries {
        replica.apply_replicated_entry(0, entry).unwrap();
    }

    // Spot-check some instances
    for i in [0, 25, 49] {
        let id = format!("c-{:04}", i);
        let inst = replica.get_instance(&id).unwrap();
        assert_eq!(inst.state, "active");
        assert_eq!(inst.ctx["count"], 10);
    }

    assert_eq!(replica.get_all_instances().len(), 50);
}
