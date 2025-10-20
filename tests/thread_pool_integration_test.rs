//! Integration tests for thread pool support

#![cfg(feature = "integration")]

use async_trait::async_trait;
use rust_network_system::{
    integration::{
        CompressionProcessor, DecompressionProcessor, EncryptionProcessor, IdentityProcessor,
        MessageProcessor, ProcessorPipeline, ThreadPoolSessionHandler,
    },
    Message, ServerConfig, Session, SessionHandler, TcpClient, TcpServer,
};
use std::sync::Arc;
use std::time::Duration;

/// Echo handler for testing
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
async fn test_identity_processor() {
    let config = ServerConfig::new("127.0.0.1:9201");
    let mut server = TcpServer::with_config(config);

    let echo_handler = Arc::new(TestEchoHandler);
    let processor = Arc::new(IdentityProcessor);
    let handler = ThreadPoolSessionHandler::new(echo_handler, processor, 2)
        .expect("Failed to create handler");

    server
        .start(Arc::new(handler))
        .await
        .expect("Failed to start server");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = TcpClient::new("127.0.0.1:9201");
    client.connect().await.expect("Failed to connect");

    let original = Message::new("Test message");
    client.send(&original).await.expect("Failed to send");

    let response = client.receive().await.expect("Failed to receive");
    assert_eq!(original.data(), response.data());

    client.disconnect().await.expect("Failed to disconnect");
    server.stop().await.expect("Failed to stop server");
}

#[tokio::test]
async fn test_compression_processor() {
    let config = ServerConfig::new("127.0.0.1:9202");
    let mut server = TcpServer::with_config(config);

    let echo_handler = Arc::new(TestEchoHandler);
    let processor = Arc::new(CompressionProcessor::new(6));
    let handler = ThreadPoolSessionHandler::new(echo_handler, processor, 2)
        .expect("Failed to create handler");

    server
        .start(Arc::new(handler))
        .await
        .expect("Failed to start server");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = TcpClient::new("127.0.0.1:9202");
    client.connect().await.expect("Failed to connect");

    // Send uncompressed data
    let original_data = "x".repeat(1000); // Highly compressible
    let original = Message::new(original_data.clone());
    client.send(&original).await.expect("Failed to send");

    // Receive compressed response
    let response = client.receive().await.expect("Failed to receive");

    // Response should be compressed (smaller)
    assert!(response.size() < original.size());

    // Decompress to verify
    let decompressor = DecompressionProcessor;
    let decompressed = decompressor
        .process(response)
        .expect("Failed to decompress");
    assert_eq!(original.data(), decompressed.data());

    client.disconnect().await.expect("Failed to disconnect");
    server.stop().await.expect("Failed to stop server");
}

#[tokio::test]
async fn test_encryption_roundtrip() {
    let key = b"test_encryption_key_32_bytes!!".to_vec();

    let encrypt = EncryptionProcessor::new(key.clone());
    let decrypt = EncryptionProcessor::new(key); // XOR is symmetric

    let original = Message::new("Secret message");
    let encrypted = encrypt.process(original.clone()).unwrap();

    // Encrypted should be different
    assert_ne!(original.data(), encrypted.data());

    // Decrypt should restore original
    let decrypted = decrypt.process(encrypted).unwrap();
    assert_eq!(original.data(), decrypted.data());
}

#[tokio::test]
async fn test_processor_pipeline() {
    let config = ServerConfig::new("127.0.0.1:9203");
    let mut server = TcpServer::with_config(config);

    let echo_handler = Arc::new(TestEchoHandler);

    // Create pipeline: Encrypt -> Compress
    let key = b"pipeline_key_16!".to_vec();
    let pipeline = ProcessorPipeline::new()
        .add(EncryptionProcessor::new(key.clone()))
        .add(CompressionProcessor::new(6));

    let handler = ThreadPoolSessionHandler::new(echo_handler, Arc::new(pipeline), 2)
        .expect("Failed to create handler");

    server
        .start(Arc::new(handler))
        .await
        .expect("Failed to start server");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = TcpClient::new("127.0.0.1:9203");
    client.connect().await.expect("Failed to connect");

    let original = Message::new("Pipeline test data");
    client.send(&original).await.expect("Failed to send");

    let response = client.receive().await.expect("Failed to receive");

    // Response should be encrypted and compressed
    assert_ne!(original.data(), response.data());

    // Reverse pipeline to verify: Decompress -> Decrypt
    let reverse_pipeline = ProcessorPipeline::new()
        .add(DecompressionProcessor)
        .add(EncryptionProcessor::new(key)); // XOR decrypt

    let restored = reverse_pipeline.process(response).unwrap();
    assert_eq!(original.data(), restored.data());

    client.disconnect().await.expect("Failed to disconnect");
    server.stop().await.expect("Failed to stop server");
}

#[tokio::test]
async fn test_concurrent_processing() {
    let config = ServerConfig::new("127.0.0.1:9204");
    let mut server = TcpServer::with_config(config);

    let echo_handler = Arc::new(TestEchoHandler);
    let processor = Arc::new(CompressionProcessor::new(6));

    // Use 4 threads for concurrent processing
    let handler = ThreadPoolSessionHandler::new(echo_handler, processor, 4)
        .expect("Failed to create handler");

    server
        .start(Arc::new(handler))
        .await
        .expect("Failed to start server");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Create multiple clients
    let mut handles = Vec::new();

    for i in 0..10 {
        let handle = tokio::spawn(async move {
            let mut client = TcpClient::new("127.0.0.1:9204");
            client.connect().await.expect("Failed to connect");

            let data = format!("Message {} - {}", i, "data".repeat(100));
            let msg = Message::new(data.clone());
            client.send(&msg).await.expect("Failed to send");

            let response = client.receive().await.expect("Failed to receive");

            // Verify response is compressed
            assert!(response.size() < msg.size());

            client.disconnect().await.expect("Failed to disconnect");
        });

        handles.push(handle);
    }

    // Wait for all clients
    for handle in handles {
        handle.await.expect("Client task failed");
    }

    server.stop().await.expect("Failed to stop server");
}

#[test]
fn test_processor_error_handling() {
    let processor = CompressionProcessor::new(6);

    // Empty message should work
    let empty = Message::new(Vec::<u8>::new());
    let result = processor.process(empty);
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_large_message_processing() {
    let config = ServerConfig::new("127.0.0.1:9205");
    let mut server = TcpServer::with_config(config);

    let echo_handler = Arc::new(TestEchoHandler);
    let processor = Arc::new(CompressionProcessor::new(6));
    let handler = ThreadPoolSessionHandler::new(echo_handler, processor, 2)
        .expect("Failed to create handler");

    server
        .start(Arc::new(handler))
        .await
        .expect("Failed to start server");

    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut client = TcpClient::new("127.0.0.1:9205");
    client.connect().await.expect("Failed to connect");

    // Send 1MB of highly compressible data
    let large_data = "x".repeat(1024 * 1024);
    let original_size = large_data.len();
    let original = Message::new(large_data);

    client.send(&original).await.expect("Failed to send");

    let response = client.receive().await.expect("Failed to receive");

    // Should be significantly compressed
    let compression_ratio = response.size() as f64 / original_size as f64;
    println!("Compression ratio: {:.2}%", compression_ratio * 100.0);
    assert!(compression_ratio < 0.1); // At least 90% compression

    client.disconnect().await.expect("Failed to disconnect");
    server.stop().await.expect("Failed to stop server");
}
