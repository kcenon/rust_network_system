//! Example server using thread pool for CPU-intensive message processing

use async_trait::async_trait;
use rust_network_system::{Message, Session, SessionHandler};

#[cfg(feature = "integration")]
use rust_network_system::integration::{
    CompressionProcessor, DecompressionProcessor, ProcessorPipeline, ThreadPoolSessionHandler,
};

/// Echo handler that sends back received messages
#[allow(dead_code)] // Used only when 'integration' feature is enabled
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
        println!("Session {} received {} bytes", session.id(), message.size());

        // Echo the processed message back
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

#[cfg(feature = "integration")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();

    println!("=== Thread Pool Enhanced Network Server ===\n");

    // Create server configuration
    let config = ServerConfig::new("127.0.0.1:8080").with_max_connections(100);

    // Create server
    let mut server = TcpServer::with_config(config);

    // Create message processing pipeline
    // This runs in the thread pool (CPU-intensive work offloaded from tokio)
    let pipeline = ProcessorPipeline::new()
        .add(DecompressionProcessor) // Decompress incoming messages
        .add(CompressionProcessor::new(6)); // Re-compress for echo

    let processor = Arc::new(pipeline);

    // Wrap echo handler with thread pool handler
    // CPU-intensive processing happens in thread pool, not tokio runtime
    let echo_handler = Arc::new(EchoHandler);
    let thread_pool_handler = ThreadPoolSessionHandler::new(
        echo_handler,
        processor,
        8, // 8 worker threads for CPU-intensive work
    )?;

    println!("Starting server with thread pool integration...");
    println!("- Thread pool size: 8 workers");
    println!("- Processing pipeline: Decompression -> Compression");
    println!("- Listening on: 127.0.0.1:8080\n");

    // Start server
    server.start(Arc::new(thread_pool_handler)).await?;

    println!("Server started! Press Ctrl+C to stop.\n");
    println!("Try connecting with:");
    println!("  cargo run --example simple_client --features integration\n");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;

    println!("\nShutting down...");
    server.stop().await?;

    println!("Server stopped.");

    Ok(())
}

#[cfg(not(feature = "integration"))]
fn main() {
    eprintln!("This example requires the 'integration' feature.");
    eprintln!("Run with: cargo run --example thread_pool_server --features integration");
    std::process::exit(1);
}
