//! Tests for client reconnection logic

use async_trait::async_trait;
use rust_network_system::core::{ClientConfig, Message, ServerConfig, TcpClient, TcpServer};
use rust_network_system::session::{Session, SessionHandler};
use std::sync::Arc;
use std::time::Duration;
use tokio::time::sleep;

/// Simple echo handler for testing
struct TestEchoHandler;

#[async_trait]
impl SessionHandler for TestEchoHandler {
    async fn on_connected(&self, _session: &Session) {
        tracing::info!("Client connected");
    }

    async fn on_message(&self, session: &Session, message: Message) {
        // Echo back
        let _ = session.send(&message).await;
    }

    async fn on_disconnected(&self, _session: &Session) {
        tracing::info!("Client disconnected");
    }
}

/// Test basic reconnection after server restart
#[tokio::test]
async fn test_basic_reconnection() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .ok();

    let port = 9001;
    let addr = format!("127.0.0.1:{}", port);

    // Start server
    let mut server = TcpServer::with_config(ServerConfig::new(&addr));
    let handler = Arc::new(TestEchoHandler);
    server.start(handler.clone()).await.unwrap();

    sleep(Duration::from_millis(100)).await;

    // Create client with reconnection enabled
    let mut client = TcpClient::with_config(
        &addr,
        ClientConfig::new()
            .with_auto_reconnect(true)
            .with_connect_timeout(Duration::from_secs(2)),
    );

    // Initial connection
    client.connect().await.unwrap();
    assert!(client.is_connected());

    // Send a message to verify connection works
    let msg = Message::new("test before restart");
    client.send(&msg).await.unwrap();

    // Stop server (simulates connection loss)
    server.stop().await.unwrap();
    sleep(Duration::from_millis(200)).await;

    // Restart server
    let mut server = TcpServer::with_config(ServerConfig::new(&addr));
    server.start(handler).await.unwrap();
    sleep(Duration::from_millis(100)).await;

    // Try to reconnect manually
    match client.reconnect().await {
        Ok(()) => {
            tracing::info!("Reconnection successful");
            assert!(client.is_connected());
        }
        Err(e) => {
            panic!("Failed to reconnect: {}", e);
        }
    }

    // Verify connection works after reconnection
    let msg = Message::new("test after reconnect");
    client.send(&msg).await.unwrap();

    // Cleanup
    client.disconnect().await.unwrap();
    server.stop().await.unwrap();
}

/// Test automatic reconnection on send failure
#[tokio::test]
async fn test_auto_reconnect_on_send() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .ok();

    let port = 9002;
    let addr = format!("127.0.0.1:{}", port);

    // Start server
    let mut server = TcpServer::with_config(ServerConfig::new(&addr));
    let handler = Arc::new(TestEchoHandler);
    server.start(handler.clone()).await.unwrap();

    sleep(Duration::from_millis(100)).await;

    // Create client with auto-reconnect enabled
    let mut client = TcpClient::with_config(
        &addr,
        ClientConfig::new()
            .with_auto_reconnect(true)
            .with_connect_timeout(Duration::from_secs(2)),
    );

    client.connect().await.unwrap();

    // Stop server
    server.stop().await.unwrap();
    sleep(Duration::from_millis(200)).await;

    // Restart server
    let mut server = TcpServer::with_config(ServerConfig::new(&addr));
    server.start(handler).await.unwrap();
    sleep(Duration::from_millis(100)).await;

    // Try send_with_reconnect - should automatically reconnect
    let msg = Message::new("auto reconnect test");
    match client.send_with_reconnect(&msg).await {
        Ok(()) => {
            tracing::info!("Send with auto-reconnect successful");
            assert!(client.is_connected());
        }
        Err(e) => {
            panic!("Failed to send with auto-reconnect: {}", e);
        }
    }

    // Cleanup
    client.disconnect().await.unwrap();
    server.stop().await.unwrap();
}

/// Test reconnection with max attempts limit
#[tokio::test]
async fn test_reconnection_max_attempts() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .ok();

    let port = 9003;
    let addr = format!("127.0.0.1:{}", port);

    // Create client with limited reconnect attempts
    let mut client = TcpClient::with_config(&addr, ClientConfig::new().with_auto_reconnect(true));

    // Try to connect to non-existent server - should fail after max attempts
    match client.reconnect().await {
        Ok(()) => {
            panic!("Should have failed after max attempts");
        }
        Err(e) => {
            tracing::info!("Expected failure: {}", e);
            assert!(e.to_string().contains("Failed to reconnect"));
        }
    }
}

/// Test exponential backoff in reconnection
#[tokio::test]
async fn test_exponential_backoff() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .ok();

    let port = 9004;
    let addr = format!("127.0.0.1:{}", port);

    // Create client with reconnection
    let mut client = TcpClient::with_config(&addr, ClientConfig::new().with_auto_reconnect(true));

    // Measure time for reconnection attempts
    let start = tokio::time::Instant::now();

    // Try to connect to non-existent server
    let _result = client.reconnect().await;

    let elapsed = start.elapsed();

    // With exponential backoff (100ms, 200ms delays) + jitter (0-100ms each):
    // Total delay should be at least 300ms (100+200) for delays between attempts
    // Connection attempts fail immediately (Connection refused), so we mainly
    // measure the backoff delays, not connection timeouts
    assert!(
        elapsed >= Duration::from_millis(300),
        "Exponential backoff should take at least 300ms (actual backoff delays), took {:?}",
        elapsed
    );

    tracing::info!(
        "Reconnection attempts took {:?} (backoff working correctly)",
        elapsed
    );
}

/// Test reconnection preserves client configuration
#[tokio::test]
async fn test_reconnection_preserves_config() {
    tracing_subscriber::fmt()
        .with_test_writer()
        .with_max_level(tracing::Level::DEBUG)
        .try_init()
        .ok();

    let port = 9005;
    let addr = format!("127.0.0.1:{}", port);

    // Start server
    let mut server = TcpServer::with_config(ServerConfig::new(&addr));
    let handler = Arc::new(TestEchoHandler);
    server.start(handler.clone()).await.unwrap();

    sleep(Duration::from_millis(100)).await;

    // Create client with custom config
    let mut client = TcpClient::with_config(
        &addr,
        ClientConfig::new()
            .with_auto_reconnect(true)
            .with_read_timeout(Duration::from_secs(5))
            .with_write_timeout(Duration::from_secs(3))
            .with_connect_timeout(Duration::from_secs(2)),
    );

    client.connect().await.unwrap();
    let initial_addr = client.address().to_string();

    // Stop and restart server
    server.stop().await.unwrap();
    sleep(Duration::from_millis(200)).await;

    let mut server = TcpServer::with_config(ServerConfig::new(&addr));
    server.start(handler).await.unwrap();
    sleep(Duration::from_millis(100)).await;

    // Reconnect
    client.reconnect().await.unwrap();

    // Verify configuration is preserved
    assert_eq!(client.address(), initial_addr);
    assert!(client.is_connected());

    // Verify timeouts still work (send should complete within timeout)
    let msg = Message::new("config test");
    client.send(&msg).await.unwrap();

    // Cleanup
    client.disconnect().await.unwrap();
    server.stop().await.unwrap();
}
