//! End-to-end client-server benchmarks.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rstmdb_client::{Client, ConnectionConfig};
use rstmdb_core::StateMachineEngine;
use rstmdb_server::{Server, ServerConfig};
use rstmdb_wal::{FsyncPolicy, WalConfig};
use std::sync::Arc;
use tempfile::TempDir;
use tokio::runtime::Runtime;

struct TestSetup {
    _dir: TempDir,
    _server_handle: tokio::task::JoinHandle<()>,
    client: Client,
}

fn setup_server_and_client(rt: &Runtime) -> TestSetup {
    let dir = TempDir::new().unwrap();
    let wal_config = WalConfig::new(dir.path())
        .with_segment_size(64 * 1024 * 1024)
        .with_fsync_policy(FsyncPolicy::Never);
    let engine = Arc::new(StateMachineEngine::new(wal_config).unwrap());

    // Find available port
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let server_config = ServerConfig::new(addr);
    let server = Arc::new(Server::new(server_config, engine));

    // Start server
    let server_clone = server.clone();
    let server_handle = rt.spawn(async move {
        let _ = server_clone.run().await;
    });

    // Give server time to start
    std::thread::sleep(std::time::Duration::from_millis(100));

    // Connect client
    let client_config = ConnectionConfig::new(addr).with_client_name("bench");
    let client = Client::new(client_config);

    rt.block_on(async {
        client.connect().await.unwrap();

        // Spawn read loop
        let conn = client.connection();
        tokio::spawn(async move {
            let _ = conn.read_loop().await;
        });
        tokio::task::yield_now().await;
    });

    TestSetup {
        _dir: dir,
        _server_handle: server_handle,
        client,
    }
}

fn bench_ping_latency(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let setup = setup_server_and_client(&rt);

    let mut group = c.benchmark_group("e2e_ping");
    group.throughput(Throughput::Elements(1));

    group.bench_function("ping", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(setup.client.ping().await.unwrap()) });
    });

    group.finish();
}

fn bench_create_instance_e2e(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let setup = setup_server_and_client(&rt);

    // Register machine
    rt.block_on(async {
        setup
            .client
            .put_machine(
                "bench",
                1,
                serde_json::json!({
                    "states": ["created", "done"],
                    "initial": "created",
                    "transitions": [{"from": "created", "event": "FINISH", "to": "done"}]
                }),
            )
            .await
            .unwrap();
    });

    let mut group = c.benchmark_group("e2e_create_instance");
    group.throughput(Throughput::Elements(1));

    let mut id = 0u64;
    group.bench_function("create", |b| {
        b.to_async(&rt).iter(|| {
            id += 1;
            let client = &setup.client;
            async move {
                black_box(
                    client
                        .create_instance("bench", 1, Some(&format!("e2e-inst-{}", id)), None, None)
                        .await
                        .unwrap(),
                )
            }
        });
    });

    group.finish();
}

fn bench_apply_event_e2e(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let setup = setup_server_and_client(&rt);

    // Register machine and create instances
    rt.block_on(async {
        setup
            .client
            .put_machine(
                "bench",
                1,
                serde_json::json!({
                    "states": ["s1", "s2", "s3", "s4", "s5"],
                    "initial": "s1",
                    "transitions": [
                        {"from": "s1", "event": "NEXT", "to": "s2"},
                        {"from": "s2", "event": "NEXT", "to": "s3"},
                        {"from": "s3", "event": "NEXT", "to": "s4"},
                        {"from": "s4", "event": "NEXT", "to": "s5"},
                        {"from": "s5", "event": "NEXT", "to": "s1"},
                    ]
                }),
            )
            .await
            .unwrap();

        // Pre-create instances
        for i in 0..100 {
            setup
                .client
                .create_instance("bench", 1, Some(&format!("apply-inst-{}", i)), None, None)
                .await
                .unwrap();
        }
    });

    let mut group = c.benchmark_group("e2e_apply_event");
    group.throughput(Throughput::Elements(1));

    let mut id = 0usize;
    group.bench_function("apply", |b| {
        b.to_async(&rt).iter(|| {
            id = (id + 1) % 100;
            let client = &setup.client;
            let instance_id = format!("apply-inst-{}", id);
            async move {
                black_box(
                    client
                        .apply_event(&instance_id, "NEXT", None, None, None)
                        .await,
                )
            }
        });
    });

    group.finish();
}

fn bench_concurrent_requests(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let setup = setup_server_and_client(&rt);

    // Register machine
    rt.block_on(async {
        setup
            .client
            .put_machine(
                "bench",
                1,
                serde_json::json!({
                    "states": ["created", "done"],
                    "initial": "created",
                    "transitions": [{"from": "created", "event": "FINISH", "to": "done"}]
                }),
            )
            .await
            .unwrap();
    });

    let mut group = c.benchmark_group("e2e_concurrent");
    group.sample_size(20);

    for concurrency in [1, 10, 50] {
        group.throughput(Throughput::Elements(concurrency as u64));
        group.bench_with_input(
            BenchmarkId::new("pings", concurrency),
            &concurrency,
            |b, &conc| {
                b.to_async(&rt).iter(|| {
                    let client = &setup.client;
                    async move {
                        let futures: Vec<_> = (0..conc).map(|_| client.ping()).collect();
                        black_box(futures::future::join_all(futures).await)
                    }
                });
            },
        );
    }

    group.finish();
}

fn bench_roundtrip_latency(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let setup = setup_server_and_client(&rt);

    // Register machine
    rt.block_on(async {
        setup
            .client
            .put_machine(
                "bench",
                1,
                serde_json::json!({
                    "states": ["created", "done"],
                    "initial": "created",
                    "transitions": [{"from": "created", "event": "FINISH", "to": "done"}]
                }),
            )
            .await
            .unwrap();
    });

    let mut group = c.benchmark_group("e2e_latency");

    // Measure full roundtrip for different operations
    group.bench_function("info", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(setup.client.info().await.unwrap()) });
    });

    group.bench_function("list_machines", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(setup.client.list_machines().await.unwrap()) });
    });

    group.bench_function("get_machine", |b| {
        b.to_async(&rt)
            .iter(|| async { black_box(setup.client.get_machine("bench", 1).await.unwrap()) });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_ping_latency,
    bench_create_instance_e2e,
    bench_apply_event_e2e,
    bench_concurrent_requests,
    bench_roundtrip_latency,
);

criterion_main!(benches);
