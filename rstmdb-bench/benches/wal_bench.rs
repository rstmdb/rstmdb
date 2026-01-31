//! WAL benchmarks.

use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rstmdb_wal::entry::WalEntry;
use rstmdb_wal::{FsyncPolicy, Wal, WalConfig, WalOffset};
use tempfile::TempDir;

fn create_test_wal(fsync: FsyncPolicy) -> (TempDir, Wal) {
    let dir = TempDir::new().unwrap();
    let config = WalConfig::new(dir.path())
        .with_segment_size(64 * 1024 * 1024) // 64MB segments
        .with_fsync_policy(fsync);
    let wal = Wal::open(config).unwrap();
    (dir, wal)
}

fn create_test_entry(size: usize) -> WalEntry {
    WalEntry::ApplyEvent {
        instance_id: "bench-instance".to_string(),
        event: "PROCESS".to_string(),
        from_state: "pending".to_string(),
        to_state: "completed".to_string(),
        payload: serde_json::json!({
            "data": "x".repeat(size),
        }),
        ctx: serde_json::json!({}),
        event_id: None,
        idempotency_key: None,
    }
}

fn bench_wal_append(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_append");

    // Test with different fsync policies
    for (name, policy) in [
        ("no_fsync", FsyncPolicy::Never),
        ("fsync_every_100", FsyncPolicy::EveryN(100)),
    ] {
        let (_dir, wal) = create_test_wal(policy);
        let entry = create_test_entry(100);

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::new("small_entry", name), &entry, |b, entry| {
            b.iter(|| black_box(wal.append(entry).unwrap()));
        });
    }

    // Test with different payload sizes (no fsync for speed)
    let (_dir, wal) = create_test_wal(FsyncPolicy::Never);
    for size in [100, 1000, 10000] {
        let entry = create_test_entry(size);
        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(
            BenchmarkId::new("payload_bytes", size),
            &entry,
            |b, entry| {
                b.iter(|| black_box(wal.append(entry).unwrap()));
            },
        );
    }

    group.finish();
}

fn bench_wal_append_batch(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_append_batch");

    let (_dir, wal) = create_test_wal(FsyncPolicy::Never);
    let entry = create_test_entry(100);

    for batch_size in [10, 100, 1000] {
        group.throughput(Throughput::Elements(batch_size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(batch_size),
            &batch_size,
            |b, &size| {
                b.iter(|| {
                    for _ in 0..size {
                        black_box(wal.append(&entry).unwrap());
                    }
                });
            },
        );
    }

    group.finish();
}

fn bench_wal_read(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_read");

    // Pre-populate WAL
    let (_dir, wal) = create_test_wal(FsyncPolicy::Never);
    let entry = create_test_entry(100);

    for _ in 0..1000 {
        wal.append(&entry).unwrap();
    }

    // Benchmark reading
    for limit in [10, 100, 1000] {
        group.throughput(Throughput::Elements(limit as u64));
        group.bench_with_input(BenchmarkId::new("entries", limit), &limit, |b, &limit| {
            b.iter(|| black_box(wal.read_from(WalOffset::new(1, 0), Some(limit)).unwrap()));
        });
    }

    group.finish();
}

fn bench_wal_recovery(c: &mut Criterion) {
    let mut group = c.benchmark_group("wal_recovery");

    for entry_count in [100, 1000, 10000] {
        // Create and populate a WAL
        let dir = TempDir::new().unwrap();
        {
            let config = WalConfig::new(dir.path())
                .with_segment_size(64 * 1024 * 1024)
                .with_fsync_policy(FsyncPolicy::Never);
            let wal = Wal::open(config).unwrap();
            let entry = create_test_entry(100);

            for _ in 0..entry_count {
                wal.append(&entry).unwrap();
            }
            wal.sync().unwrap();
        }

        // Benchmark recovery
        group.throughput(Throughput::Elements(entry_count as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(entry_count),
            &entry_count,
            |b, _| {
                b.iter(|| {
                    let config = WalConfig::new(dir.path())
                        .with_segment_size(64 * 1024 * 1024)
                        .with_fsync_policy(FsyncPolicy::Never);
                    black_box(Wal::open(config).unwrap())
                });
            },
        );
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_wal_append,
    bench_wal_append_batch,
    bench_wal_read,
    bench_wal_recovery,
);

criterion_main!(benches);
