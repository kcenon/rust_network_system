//! Example client that sends compressed messages

#[cfg(feature = "integration")]
use rust_network_system::integration::{
    CompressionProcessor, DecompressionProcessor, MessageProcessor,
};

#[cfg(feature = "integration")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("=== Compression Client Example ===\n");

    // Create client configuration
    let config = ClientConfig::new()
        .with_connect_timeout(Duration::from_secs(5))
        .with_read_timeout(Duration::from_secs(10));

    // Create client
    let mut client = TcpClient::with_config("127.0.0.1:8080", config);

    println!("Connecting to server...");
    client.connect().await?;
    println!("Connected!\n");

    // Create processors
    let compressor = CompressionProcessor::new(6);
    let decompressor = DecompressionProcessor;

    // Send compressed messages
    for i in 1..=5 {
        let original_data = format!("Hello from client! Message #{} - {}", i, "x".repeat(100));
        let original_size = original_data.len();

        println!("Sending message #{}:", i);
        println!("  Original size: {} bytes", original_size);

        // Compress message
        let msg = Message::new(original_data.clone());
        let compressed_msg = compressor.process(msg)?;
        let compressed_size = compressed_msg.size();

        println!(
            "  Compressed size: {} bytes ({:.1}% reduction)",
            compressed_size,
            (1.0 - compressed_size as f64 / original_size as f64) * 100.0
        );

        // Send compressed message
        client.send(&compressed_msg).await?;

        // Receive response (also compressed by server)
        let response = client.receive().await?;
        println!("  Received: {} bytes (compressed)", response.size());

        // Decompress response
        let decompressed = decompressor.process(response)?;
        let response_text = String::from_utf8_lossy(decompressed.data());
        println!("  Decompressed: {} bytes", decompressed.size());
        println!(
            "  Content matches: {}\n",
            response_text.starts_with("Hello from client!")
        );

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    println!("Disconnecting...");
    client.disconnect().await?;

    println!("Done!");

    Ok(())
}

#[cfg(not(feature = "integration"))]
fn main() {
    eprintln!("This example requires the 'integration' feature.");
    eprintln!("Run with: cargo run --example compression_client --features integration");
    std::process::exit(1);
}
