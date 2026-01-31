//! State machine engine benchmarks.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rstmdb_core::StateMachineEngine;
use rstmdb_wal::{FsyncPolicy, WalConfig};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

// Global counter to ensure unique instance IDs across all benchmark iterations
static INSTANCE_COUNTER: AtomicU64 = AtomicU64::new(0);

fn create_test_engine() -> (TempDir, Arc<StateMachineEngine>) {
    let dir = TempDir::new().unwrap();
    let config = WalConfig::new(dir.path())
        .with_segment_size(64 * 1024 * 1024)
        .with_fsync_policy(FsyncPolicy::Never);
    let engine = Arc::new(StateMachineEngine::new(config).unwrap());
    (dir, engine)
}

fn setup_machine(engine: &StateMachineEngine) {
    let definition = serde_json::json!({
        "states": ["created", "processing", "completed", "failed"],
        "initial": "created",
        "transitions": [
            {"from": "created", "event": "START", "to": "processing"},
            {"from": "processing", "event": "COMPLETE", "to": "completed"},
            {"from": "processing", "event": "FAIL", "to": "failed"},
            {"from": "failed", "event": "RETRY", "to": "processing"},
        ]
    });
    engine.put_machine("benchmark", 1, &definition).unwrap();
}

fn bench_put_machine(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_put_machine");

    let (_dir, engine) = create_test_engine();

    // Simple machine
    let simple_def = serde_json::json!({
        "states": ["a", "b"],
        "initial": "a",
        "transitions": [{"from": "a", "event": "GO", "to": "b"}]
    });

    group.bench_function("simple", |b| {
        let mut version = 1u32;
        b.iter(|| {
            version += 1;
            black_box(engine.put_machine("simple", version, &simple_def).unwrap())
        });
    });

    // Complex machine with many states and transitions
    let complex_def = serde_json::json!({
        "states": (0..20).map(|i| format!("state_{}", i)).collect::<Vec<_>>(),
        "initial": "state_0",
        "transitions": (0..19).map(|i| serde_json::json!({
            "from": format!("state_{}", i),
            "event": format!("NEXT_{}", i),
            "to": format!("state_{}", i + 1)
        })).collect::<Vec<_>>()
    });

    group.bench_function("complex", |b| {
        let mut version = 1u32;
        b.iter(|| {
            version += 1;
            black_box(
                engine
                    .put_machine("complex", version, &complex_def)
                    .unwrap(),
            )
        });
    });

    group.finish();
}

fn bench_create_instance(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_create_instance");

    let (_dir, engine) = create_test_engine();
    setup_machine(&engine);

    group.throughput(Throughput::Elements(1));
    group.bench_function("create", |b| {
        b.iter(|| {
            let id = INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
            black_box(
                engine
                    .create_instance(
                        &format!("inst-{}", id),
                        "benchmark",
                        1,
                        serde_json::Value::Null,
                        None,
                    )
                    .unwrap(),
            )
        });
    });

    // With initial context
    group.bench_function("create_with_ctx", |b| {
        let ctx = serde_json::json!({"user_id": "u-123", "amount": 100});
        b.iter(|| {
            let id = INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
            black_box(
                engine
                    .create_instance(
                        &format!("inst-ctx-{}", id),
                        "benchmark",
                        1,
                        ctx.clone(),
                        None,
                    )
                    .unwrap(),
            )
        });
    });

    group.finish();
}

fn bench_get_instance(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_get_instance");

    let (_dir, engine) = create_test_engine();
    setup_machine(&engine);

    // Pre-create instances
    for i in 0..1000 {
        engine
            .create_instance(
                &format!("get-inst-{}", i),
                "benchmark",
                1,
                serde_json::Value::Null,
                None,
            )
            .unwrap();
    }

    group.throughput(Throughput::Elements(1));
    group.bench_function("get", |b| {
        let mut id = 0usize;
        b.iter(|| {
            id = (id + 1) % 1000;
            black_box(engine.get_instance(&format!("get-inst-{}", id)).unwrap())
        });
    });

    group.finish();
}

fn bench_apply_event(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_apply_event");

    let (_dir, engine) = create_test_engine();
    setup_machine(&engine);

    // Pre-create instances in "created" state
    for i in 0..10000 {
        engine
            .create_instance(
                &format!("event-inst-{}", i),
                "benchmark",
                1,
                serde_json::Value::Null,
                None,
            )
            .unwrap();
    }

    group.throughput(Throughput::Elements(1));

    // Single transition
    group.bench_function("single_transition", |b| {
        let mut id = 0usize;
        b.iter(|| {
            id = (id + 1) % 10000;
            let instance_id = format!("event-inst-{}", id);
            // Reset to created state by getting current state
            let instance = engine.get_instance(&instance_id).unwrap();
            if instance.state != "created" {
                // Skip if not in created state (will cycle through states)
            }
            black_box(engine.apply_event(
                &instance_id,
                "START",
                serde_json::Value::Null,
                None,
                None,
                None,
                None,
            ))
        });
    });

    // With payload
    group.bench_function("with_payload", |b| {
        let mut id = 0usize;
        let payload = serde_json::json!({"action": "process", "data": "x".repeat(100)});
        b.iter(|| {
            id = (id + 1) % 10000;
            black_box(engine.apply_event(
                &format!("event-inst-{}", id),
                "START",
                payload.clone(),
                None,
                None,
                None,
                None,
            ))
        });
    });

    group.finish();
}

fn bench_apply_event_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_throughput");
    group.sample_size(20);

    let (_dir, engine) = create_test_engine();
    setup_machine(&engine);

    // Measure how many events we can apply per second
    for batch_size in [100, 1000] {
        // Use unique prefix for this batch
        let batch_prefix = INSTANCE_COUNTER.fetch_add(batch_size as u64, Ordering::Relaxed);

        // Pre-create instances
        for i in 0..batch_size {
            engine
                .create_instance(
                    &format!("tp-inst-{}-{}", batch_prefix, i),
                    "benchmark",
                    1,
                    serde_json::Value::Null,
                    None,
                )
                .unwrap();
        }

        let prefix = batch_prefix;
        group.throughput(Throughput::Elements(batch_size as u64));
        group.bench_with_input(
            BenchmarkId::new("events", batch_size),
            &batch_size,
            |b, &size| {
                b.iter(|| {
                    for i in 0..size {
                        let _ = engine.apply_event(
                            &format!("tp-inst-{}-{}", prefix, i),
                            "START",
                            serde_json::Value::Null,
                            None,
                            None,
                            None,
                            None,
                        );
                    }
                });
            },
        );
    }

    group.finish();
}

fn bench_guard_evaluation(c: &mut Criterion) {
    let mut group = c.benchmark_group("engine_guard");

    let (_dir, engine) = create_test_engine();

    // Machine with guard on single transition
    let definition = serde_json::json!({
        "states": ["pending", "approved"],
        "initial": "pending",
        "transitions": [
            {
                "from": "pending",
                "event": "APPROVE",
                "to": "approved",
                "guard": "ctx.amount <= 1000"
            }
        ]
    });
    engine.put_machine("guarded", 1, &definition).unwrap();

    group.bench_function("simple_guard", |b| {
        b.iter(|| {
            // Create new instance for each iteration
            let id = INSTANCE_COUNTER.fetch_add(1, Ordering::Relaxed);
            let instance_id = format!("guard-bench-{}", id);
            engine
                .create_instance(
                    &instance_id,
                    "guarded",
                    1,
                    serde_json::json!({"amount": 500}),
                    None,
                )
                .unwrap();
            black_box(engine.apply_event(
                &instance_id,
                "APPROVE",
                serde_json::Value::Null,
                None,
                None,
                None,
                None,
            ))
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_put_machine,
    bench_create_instance,
    bench_get_instance,
    bench_apply_event,
    bench_apply_event_throughput,
    bench_guard_evaluation,
);

criterion_main!(benches);
