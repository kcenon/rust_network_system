//! Simple echo server example

use async_trait::async_trait;
use rust_network_system::{Message, ServerConfig, Session, SessionHandler, TcpServer};
use std::sync::Arc;

/// Echo handler that sends back received messages
struct EchoHandler;

#[async_trait]
impl SessionHandler for EchoHandler {
    async fn on_connected(&self, session: &Session) {
        println!(
            "Client connected: {} (ID: {})",
            session.peer_addr(),
            session.id()
        );
    }

    async fn on_message(&self, session: &Session, message: Message) {
        println!(
            "Session {} received: {}",
            session.id(),
            String::from_utf8_lossy(message.data())
        );

        // Echo the message back
        if let Err(e) = session.send(&message).await {
            eprintln!("Failed to send echo: {}", e);
        }
    }

    async fn on_disconnected(&self, session: &Session) {
        println!(
            "Client disconnected: {} (uptime: {:?})",
            session.id(),
            session.uptime()
        );
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    // Create server configuration
    let config = ServerConfig::new("127.0.0.1:8080").with_max_connections(100);

    // Create server
    let mut server = TcpServer::with_config(config);

    println!("Starting echo server on 127.0.0.1:8080...");

    // Start server with echo handler
    let handler = Arc::new(EchoHandler);
    server.start(handler).await?;

    println!("Server started! Press Ctrl+C to stop.");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;

    println!("\nShutting down...");
    server.stop().await?;

    println!("Server stopped.");

    Ok(())
}
