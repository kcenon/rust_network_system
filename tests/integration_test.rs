//! Integration tests for the network system

use async_trait::async_trait;
use rust_network_system::{
    DefaultSessionHandler, Message, ServerConfig, Session, SessionHandler, TcpClient, TcpServer,
};
use std::sync::Arc;
use std::time::Duration;

/// Test handler that echoes messages back
struct TestEchoHandler;

#[async_trait]
impl SessionHandler for TestEchoHandler {
    async fn on_connected(&self, _session: &Session) {}

    async fn on_message(&self, session: &Session, message: Message) {
        let _ = session.send(&message).await;
    }

    async fn on_disconnected(&self, _session: &Session) {}
}

#[tokio::test]
async fn test_server_startup_shutdown() {
    let config = ServerConfig::new("127.0.0.1:9001");
    let mut server = TcpServer::with_config(config);

    assert!(!server.is_running());

    let handler = Arc::new(DefaultSessionHandler);
    server.start(handler).await.expect("Failed to start server");

    // Give server task time to set running flag
    tokio::time::sleep(Duration::from_millis(10)).await;

    assert!(server.is_running());

    server.stop().await.expect("Failed to stop server");

    assert!(!server.is_running());
}

#[tokio::test]
async fn test_client_connect_disconnect() {
    // Start server
    let config = ServerConfig::new("127.0.0.1:9002");
    let mut server = TcpServer::with_config(config);
    let handler = Arc::new(DefaultSessionHandler);
    server.start(handler).await.expect("Failed to start server");

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create and connect client
    let mut client = TcpClient::new("127.0.0.1:9002");
    assert!(!client.is_connected());

    client.connect().await.expect("Failed to connect");
    assert!(client.is_connected());

    client.disconnect().await.expect("Failed to disconnect");
    assert!(!client.is_connected());

    // Cleanup
    server.stop().await.expect("Failed to stop server");
}

#[tokio::test]
async fn test_echo_message() {
    // Start echo server
    let config = ServerConfig::new("127.0.0.1:9003");
    let mut server = TcpServer::with_config(config);
    let handler = Arc::new(TestEchoHandler);
    server.start(handler).await.expect("Failed to start server");

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect client
    let mut client = TcpClient::new("127.0.0.1:9003");
    client.connect().await.expect("Failed to connect");

    // Send message
    let original = Message::new("Hello, World!");
    client.send(&original).await.expect("Failed to send");

    // Receive echo
    let response = client.receive().await.expect("Failed to receive");

    assert_eq!(original.data(), response.data());

    // Cleanup
    client.disconnect().await.expect("Failed to disconnect");
    server.stop().await.expect("Failed to stop server");
}

#[tokio::test]
async fn test_multiple_messages() {
    // Start echo server
    let config = ServerConfig::new("127.0.0.1:9004");
    let mut server = TcpServer::with_config(config);
    let handler = Arc::new(TestEchoHandler);
    server.start(handler).await.expect("Failed to start server");

    // Give server time to start
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Connect client
    let mut client = TcpClient::new("127.0.0.1:9004");
    client.connect().await.expect("Failed to connect");

    // Send multiple messages
    for i in 0..10 {
        let msg = Message::new(format!("Message {}", i));
        client.send(&msg).await.expect("Failed to send");

        let response = client.receive().await.expect("Failed to receive");
        assert_eq!(msg.data(), response.data());
    }

    // Cleanup
    client.disconnect().await.expect("Failed to disconnect");
    server.stop().await.expect("Failed to stop server");
}

#[tokio::test]
async fn test_json_serialization() {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Serialize, Deserialize, PartialEq)]
    struct TestData {
        id: u32,
        name: String,
        values: Vec<i32>,
    }

    let data = TestData {
        id: 42,
        name: "test".to_string(),
        values: vec![1, 2, 3, 4, 5],
    };

    let message = Message::from_json(&data).expect("Failed to serialize");
    let decoded: TestData = message.to_json().expect("Failed to deserialize");

    assert_eq!(data, decoded);
}

#[tokio::test]
async fn test_connection_count() {
    // Start server
    let config = ServerConfig::new("127.0.0.1:9005").with_max_connections(5);
    let mut server = TcpServer::with_config(config);
    let handler = Arc::new(DefaultSessionHandler);
    server.start(handler).await.expect("Failed to start server");

    tokio::time::sleep(Duration::from_millis(100)).await;

    assert_eq!(server.connection_count(), 0);

    // Connect multiple clients
    let mut clients = Vec::new();
    for _ in 0..3 {
        let mut client = TcpClient::new("127.0.0.1:9005");
        client.connect().await.expect("Failed to connect");
        clients.push(client);
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // Give time for connections to register
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(server.connection_count(), 3);

    // Disconnect all
    for mut client in clients {
        client.disconnect().await.expect("Failed to disconnect");
    }

    // Give time for disconnections to register
    tokio::time::sleep(Duration::from_millis(200)).await;

    assert_eq!(server.connection_count(), 0);

    // Cleanup
    server.stop().await.expect("Failed to stop server");
}
