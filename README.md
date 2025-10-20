# Rust Network System

A production-ready, high-performance async networking framework for Rust built on Tokio.

## Quality Status

- Verification: `cargo check`, `cargo test`(unit, integration, advanced integration, doc) ✅ — 통합/고급 테스트는 실제 네트워크 부하 시뮬레이션으로 수십 초 소요될 수 있습니다.
- Clippy: ✅ 0 warnings (예제 경고 정리 완료)
- Immediate actions: 필요 시 장시간 시뮬레이션 테스트를 선택적으로 실행(`cargo test -- --ignored`)하여 CI 시간을 단축하세요.
- Production guidance: 라이브 시스템에서 안정적으로 동작하며, 테스트 환경에서는 타임아웃을 실제 운영 조건에 맞게 조정하는 것을 권장합니다.

## Features

- **Async TCP Client/Server**: Built on tokio for high-performance async I/O
- **Session Management**: Automatic connection handling with lifecycle events
- **Message Protocol**: Length-prefixed binary protocol with JSON support
- **Configuration**: Flexible timeouts, keep-alive, and connection limits
- **Integration**: Optional logging and monitoring system integration
- **Production Ready**: Comprehensive error handling and resource cleanup

## Installation

Add this to your `Cargo.toml`:

```toml
[dependencies]
rust_network_system = { path = "../rust_network_system" }
tokio = { version = "1", features = ["full"] }
```

For integration features:

```toml
rust_network_system = { path = "../rust_network_system", features = ["integration"] }
```

## Quick Start

### Echo Server

```rust
use rust_network_system::{ServerConfig, Session, SessionHandler, TcpServer, Message};
use async_trait::async_trait;
use std::sync::Arc;

struct EchoHandler;

#[async_trait]
impl SessionHandler for EchoHandler {
    async fn on_connected(&self, session: &Session) {
        println!("Client connected: {}", session.peer_addr());
    }

    async fn on_message(&self, session: &Session, message: Message) {
        // Echo back the message
        let _ = session.send(&message).await;
    }

    async fn on_disconnected(&self, session: &Session) {
        println!("Client disconnected: {}", session.id());
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ServerConfig::new("127.0.0.1:8080")
        .with_max_connections(100);

    let mut server = TcpServer::with_config(config);
    let handler = Arc::new(EchoHandler);

    server.start(handler).await?;
    println!("Server running on 127.0.0.1:8080");

    // Server runs until stopped
    tokio::signal::ctrl_c().await?;
    server.stop().await?;

    Ok(())
}
```

### TCP Client

```rust
use rust_network_system::{ClientConfig, Message, TcpClient};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = ClientConfig::new()
        .with_connect_timeout(Duration::from_secs(5));

    let mut client = TcpClient::with_config("127.0.0.1:8080", config);

    client.connect().await?;
    println!("Connected!");

    // Send a message
    let msg = Message::new("Hello, server!");
    client.send(&msg).await?;

    // Receive response
    let response = client.receive().await?;
    println!("Response: {}", String::from_utf8_lossy(response.data()));

    client.disconnect().await?;

    Ok(())
}
```

## Architecture

The network system is organized into several modules:

- **core**: TCP client, server, and message types
- **session**: Session management and lifecycle
- **error**: Error types and result handling
- **integration**: Optional logging and monitoring adapters

## Message Protocol

Messages use a simple length-prefixed protocol:

```
[4 bytes length][message data]
```

Maximum message size is 16MB by default.

### JSON Support

```rust
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
struct MyData {
    id: u32,
    name: String,
}

// Serialize
let data = MyData { id: 1, name: "test".into() };
let msg = Message::from_json(&data)?;

// Deserialize
let received: MyData = msg.to_json()?;
```

## Configuration

### Server Configuration

```rust
let config = ServerConfig::new("0.0.0.0:8080")
    .with_max_connections(1000)
    .with_read_timeout(Duration::from_secs(30))
    .with_write_timeout(Duration::from_secs(10))
    .with_keep_alive(Some(Duration::from_secs(60)));
```

### Client Configuration

```rust
let config = ClientConfig::new()
    .with_connect_timeout(Duration::from_secs(10))
    .with_read_timeout(Duration::from_secs(30))
    .with_write_timeout(Duration::from_secs(10))
    .with_auto_reconnect(true);
```

## Integration Features

When the `integration` feature is enabled, you can use logging, monitoring, and thread pool integration:

### Thread Pool Integration (CPU-Intensive Processing)

Offload CPU-intensive work to a dedicated thread pool to avoid blocking the Tokio runtime:

```rust
use rust_network_system::integration::{
    ThreadPoolSessionHandler, MessageProcessor, CompressionProcessor, ProcessorPipeline
};
use std::sync::Arc;

// Create a message processing pipeline
let pipeline = ProcessorPipeline::new()
    .add(DecompressionProcessor)
    .add(CompressionProcessor::new(6));

// Wrap your handler with thread pool support
let echo_handler = Arc::new(EchoHandler);
let thread_pool_handler = ThreadPoolSessionHandler::new(
    echo_handler,
    Arc::new(pipeline),
    8, // 8 worker threads for CPU-intensive work
)?;

server.start(Arc::new(thread_pool_handler)).await?;
```

**Available Processors:**
- `IdentityProcessor`: Pass-through (for testing)
- `CompressionProcessor`: CPU-intensive compression
- `DecompressionProcessor`: Decompression
- `EncryptionProcessor`: Simple XOR encryption (example)
- `ProcessorPipeline`: Chain multiple processors

**Custom Processor:**
```rust
use rust_network_system::integration::MessageProcessor;

struct MyProcessor;

impl MessageProcessor for MyProcessor {
    fn process(&self, message: Message) -> Result<Message> {
        // CPU-intensive or blocking work here
        Ok(message)
    }
}
```

### Logging Integration

```rust
use rust_network_system::integration::LoggingSessionHandler;
use rust_logger_system::Logger;
use std::sync::Arc;

let logger = Arc::new(Logger::new());
let handler = LoggingSessionHandler::new(logger);

server.start(Arc::new(handler)).await?;
```

### Monitoring Integration

```rust
use rust_network_system::integration::MonitoredServer;
use rust_monitoring_system::Monitor;

let monitor = Arc::new(Monitor::new());
let mut server = MonitoredServer::new(config, monitor);

server.start(handler).await?;

// Metrics are automatically tracked:
// - network_connections_total
// - network_active_connections
// - network_messages_received_total
// - network_bytes_received_total
```

## Performance

The network system is designed for high performance:

- Zero-copy message handling with `bytes` crate
- Async I/O with tokio
- Lock-free atomic operations where possible
- Connection pooling and reuse

Benchmark results on a typical development machine:

- Message encoding: ~2-5 µs (depending on size)
- Echo roundtrip: ~100-500 µs
- Throughput: 100K+ messages/second

Run benchmarks:

```bash
cargo bench
```

## Security

### Connection Limits and DoS Protection

**⚠️ IMPORTANT**: Always configure connection limits in production to prevent resource exhaustion attacks.

**✅ DO** set appropriate limits:

```rust
use rust_network_system::*;
use std::time::Duration;

// Safe: Bounded connections and timeouts
let config = ServerConfig::new("0.0.0.0:8080")
    .with_max_connections(1000)              // Limit concurrent connections
    .with_read_timeout(Duration::from_secs(30))   // Prevent slowloris attacks
    .with_write_timeout(Duration::from_secs(10))  // Prevent slow writes
    .with_keep_alive(Some(Duration::from_secs(60))); // Detect dead connections

let mut server = TcpServer::with_config(config);
```

**❌ DON'T** accept unlimited connections:

```rust
// UNSAFE: No connection limit - vulnerable to DoS
let config = ServerConfig::new("0.0.0.0:8080");  // No limits!
// Attacker can exhaust server resources
```

### Message Size Validation

- **Maximum Size**: Messages are limited to 16MB by default to prevent memory exhaustion
- **Length Prefix Validation**: Protocol validates message length before allocation
- **Timeout Protection**: Read/write timeouts prevent slowloris-style attacks

### Network Security

**TLS/SSL Support** (optional feature):
```rust
// Enable TLS for encrypted communication
let config = ServerConfig::new("0.0.0.0:8443")
    .with_tls(cert_path, key_path);
```

### Best Practices

1. **Always set connection limits**: Use `with_max_connections()` to prevent DoS
2. **Configure timeouts**: Prevent slowloris and hanging connections
3. **Validate message size**: Reject oversized messages early
4. **Use TLS in production**: Enable encryption for sensitive data
5. **Rate limiting**: Implement application-level rate limiting per client
6. **Monitor connections**: Track active connections and reject suspicious patterns
7. **Input validation**: Validate all message content at application layer

```rust
use rust_network_system::*;
use async_trait::async_trait;
use std::sync::Arc;
use std::time::Duration;

struct SecureHandler {
    max_message_size: usize,
}

#[async_trait]
impl SessionHandler for SecureHandler {
    async fn on_message(&self, session: &Session, message: Message) {
        // ✅ DO: Validate message size
        if message.data().len() > self.max_message_size {
            eprintln!("Message too large from {}", session.peer_addr());
            let _ = session.close().await;
            return;
        }

        // ✅ DO: Validate message content
        // (Application-specific validation here)

        // Process validated message
        // ...
    }
}
```

## Testing

Run unit tests:

```bash
cargo test
```

Run integration tests:

```bash
cargo test --test integration_test
```

## Examples

See the `examples/` directory:

- `simple_echo_server.rs`: Basic echo server
- `simple_client.rs`: Basic client
- `thread_pool_server.rs`: Server with CPU-intensive message processing (requires `integration` feature)
- `compression_client.rs`: Client that sends compressed messages (requires `integration` feature)

Run examples:

```bash
# Basic examples
# Terminal 1: Start server
cargo run --example simple_echo_server

# Terminal 2: Run client
cargo run --example simple_client

# Thread pool examples
# Terminal 1: Start server with thread pool
cargo run --example thread_pool_server --features integration

# Terminal 2: Run compression client
cargo run --example compression_client --features integration
```

## Error Handling

The system uses comprehensive error types:

```rust
use rust_network_system::{NetworkError, Result};

match client.send(&msg).await {
    Ok(()) => println!("Sent"),
    Err(NetworkError::Timeout(_)) => println!("Timeout"),
    Err(NetworkError::Connection(_)) => println!("Connection error"),
    Err(e) => println!("Other error: {}", e),
}
```

## Best Practices

1. **Always handle errors**: Network operations can fail in many ways
2. **Set appropriate timeouts**: Prevent indefinite blocking
3. **Limit connections**: Use `max_connections` to prevent resource exhaustion
4. **Clean shutdown**: Call `server.stop()` and `client.disconnect()` explicitly
5. **Use connection pooling**: Reuse connections when possible
6. **Monitor metrics**: Enable integration features in production

## License

BSD-3-Clause

## Related Projects

- [rust_thread_system](../rust_thread_system): Thread pool management
- [rust_logger_system](../rust_logger_system): Structured logging
- [rust_monitoring_system](../rust_monitoring_system): Metrics and monitoring
