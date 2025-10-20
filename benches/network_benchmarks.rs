//! Network system benchmarks

use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use rust_network_system::{Message, ServerConfig, Session, SessionHandler, TcpClient, TcpServer};
use std::sync::Arc;
use std::time::Duration;
use tokio::runtime::Runtime;

/// Echo handler for benchmarks
struct BenchEchoHandler;

#[async_trait]
impl SessionHandler for BenchEchoHandler {
    async fn on_connected(&self, _session: &Session) {}

    async fn on_message(&self, session: &Session, message: Message) {
        let _ = session.send(&message).await;
    }

    async fn on_disconnected(&self, _session: &Session) {}
}

fn message_encoding_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("message_encoding");

    for size in [64, 256, 1024, 4096, 16384] {
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let data = vec![0u8; size];
            let message = Message::new(data);

            b.iter(|| {
                message.encode().unwrap();
            });
        });
    }

    group.finish();
}

fn message_decoding_benchmark(c: &mut Criterion) {
    let mut group = c.benchmark_group("message_decoding");

    for size in [64, 256, 1024, 4096, 16384] {
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            let data = vec![0u8; size];
            let message = Message::new(data);
            let encoded = message.encode().unwrap();

            b.iter(|| {
                let mut buf = encoded.clone();
                Message::decode(&mut buf).unwrap();
            });
        });
    }

    group.finish();
}

fn echo_roundtrip_benchmark(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();
    let mut group = c.benchmark_group("echo_roundtrip");
    group.sample_size(50);

    // Start echo server once
    let (mut server, _port) = rt.block_on(async {
        let config = ServerConfig::new("127.0.0.1:0");
        let mut server = TcpServer::with_config(config);
        let handler = Arc::new(BenchEchoHandler);
        server.start(handler).await.expect("Failed to start server");

        // Get the actual port (using 0 for auto-assignment)
        let port = 9100; // Fixed port for benchmark
        (server, port)
    });

    // Recreate with fixed port
    rt.block_on(async {
        server.stop().await.expect("Failed to stop server");
    });

    let config = ServerConfig::new("127.0.0.1:9100");
    let mut server = rt.block_on(async {
        let mut server = TcpServer::with_config(config);
        let handler = Arc::new(BenchEchoHandler);
        server.start(handler).await.expect("Failed to start server");
        tokio::time::sleep(Duration::from_millis(100)).await;
        server
    });

    for size in [64, 256, 1024] {
        group.throughput(Throughput::Bytes(size as u64));

        group.bench_with_input(BenchmarkId::from_parameter(size), &size, |b, &size| {
            b.to_async(&rt).iter(|| async {
                let mut client = TcpClient::new("127.0.0.1:9100");
                client.connect().await.expect("Failed to connect");

                let data = vec![0u8; size];
                let message = Message::new(data);

                client.send(&message).await.expect("Failed to send");
                let _ = client.receive().await.expect("Failed to receive");

                client.disconnect().await.expect("Failed to disconnect");
            });
        });
    }

    group.finish();

    // Cleanup
    rt.block_on(async {
        server.stop().await.expect("Failed to stop server");
    });
}

fn json_serialization_benchmark(c: &mut Criterion) {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    struct TestData {
        id: u32,
        name: String,
        values: Vec<i32>,
        metadata: std::collections::HashMap<String, String>,
    }

    let mut group = c.benchmark_group("json_serialization");

    let data = TestData {
        id: 42,
        name: "benchmark_test".to_string(),
        values: (0..100).collect(),
        metadata: (0..10)
            .map(|i| (format!("key{}", i), format!("value{}", i)))
            .collect(),
    };

    group.bench_function("serialize", |b| {
        b.iter(|| {
            Message::from_json(&data).unwrap();
        });
    });

    let message = Message::from_json(&data).unwrap();

    group.bench_function("deserialize", |b| {
        b.iter(|| {
            let _: TestData = message.to_json().unwrap();
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    message_encoding_benchmark,
    message_decoding_benchmark,
    echo_roundtrip_benchmark,
    json_serialization_benchmark
);
criterion_main!(benches);
