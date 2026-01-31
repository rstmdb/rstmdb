//! Protocol encoding/decoding benchmarks.

use bytes::Bytes;
use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rstmdb_protocol::frame::Frame;
use rstmdb_protocol::message::{Operation, Request, Response};
use rstmdb_protocol::{Decoder, Encoder};

fn create_test_request(payload_size: usize) -> Request {
    Request::new("bench-1", Operation::ApplyEvent).with_params(serde_json::json!({
        "instance_id": "i-12345",
        "event": "PROCESS",
        "payload": {
            "data": "x".repeat(payload_size),
        }
    }))
}

fn create_test_response(payload_size: usize) -> Response {
    Response::ok(
        "bench-1",
        serde_json::json!({
            "from_state": "pending",
            "to_state": "completed",
            "ctx": {
                "data": "x".repeat(payload_size),
            },
            "wal_offset": 12345678,
            "applied": true,
        }),
    )
}

fn bench_frame_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_encode");

    for size in [100, 1000, 10000] {
        let payload = Bytes::from("x".repeat(size));
        let frame = Frame::new(payload.clone());

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &frame, |b, frame| {
            b.iter(|| black_box(frame.encode().unwrap()));
        });
    }

    group.finish();
}

fn bench_frame_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("frame_decode");

    for size in [100, 1000, 10000] {
        let payload = Bytes::from("x".repeat(size));
        let frame = Frame::new(payload);
        let encoded = frame.encode().unwrap();

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &encoded, |b, encoded| {
            b.iter(|| {
                let mut buf = encoded.clone();
                black_box(Frame::decode(&mut buf).unwrap())
            });
        });
    }

    group.finish();
}

fn bench_request_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("request_encode");

    for size in [100, 1000, 10000] {
        let request = create_test_request(size);

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &request, |b, request| {
            b.iter(|| black_box(Encoder::encode_request(request).unwrap()));
        });
    }

    group.finish();
}

fn bench_request_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("request_decode");

    for size in [100, 1000, 10000] {
        let request = create_test_request(size);
        let encoded = Encoder::encode_request(&request).unwrap();

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &encoded, |b, encoded| {
            b.iter(|| {
                let mut decoder = Decoder::new();
                decoder.extend(encoded);
                black_box(decoder.decode_request().unwrap())
            });
        });
    }

    group.finish();
}

fn bench_response_encode(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_encode");

    for size in [100, 1000, 10000] {
        let response = create_test_response(size);

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &response,
            |b, response| {
                b.iter(|| black_box(Encoder::encode_response(response).unwrap()));
            },
        );
    }

    group.finish();
}

fn bench_response_decode(c: &mut Criterion) {
    let mut group = c.benchmark_group("response_decode");

    for size in [100, 1000, 10000] {
        let response = create_test_response(size);
        let encoded = Encoder::encode_response(&response).unwrap();

        group.throughput(Throughput::Elements(1));
        group.bench_with_input(BenchmarkId::from_parameter(size), &encoded, |b, encoded| {
            b.iter(|| {
                let mut decoder = Decoder::new();
                decoder.extend(encoded);
                black_box(decoder.decode_response().unwrap())
            });
        });
    }

    group.finish();
}

fn bench_crc32c(c: &mut Criterion) {
    let mut group = c.benchmark_group("crc32c");

    for size in [100, 1000, 10000, 100000] {
        let data = vec![0x42u8; size];

        group.throughput(Throughput::Bytes(size as u64));
        group.bench_with_input(BenchmarkId::from_parameter(size), &data, |b, data| {
            b.iter(|| black_box(crc32c::crc32c(data)));
        });
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_frame_encode,
    bench_frame_decode,
    bench_request_encode,
    bench_request_decode,
    bench_response_encode,
    bench_response_decode,
    bench_crc32c,
);

criterion_main!(benches);
