//! Simple client example

use rust_network_system::{ClientConfig, Message, TcpClient};
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Create client configuration
    let config = ClientConfig::new()
        .with_connect_timeout(Duration::from_secs(5))
        .with_read_timeout(Duration::from_secs(10));

    // Create client
    let mut client = TcpClient::with_config("127.0.0.1:8080", config);

    println!("Connecting to server...");
    client.connect().await?;

    println!("Connected! Sending messages...");

    // Send some messages
    for i in 1..=5 {
        let msg = Message::new(format!("Hello from client! Message #{}", i));
        println!("Sending: {}", String::from_utf8_lossy(msg.data()));

        client.send(&msg).await?;

        // Receive echo
        let response = client.receive().await?;
        println!("Received: {}", String::from_utf8_lossy(response.data()));

        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    println!("Disconnecting...");
    client.disconnect().await?;

    println!("Done!");

    Ok(())
}
