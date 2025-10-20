//! TCP client implementation

use crate::core::message::Message;
use crate::error::{NetworkError, Result};
use bytes::BytesMut;
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::timeout;

/// Client configuration
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Connection timeout
    pub connect_timeout: Duration,
    /// Read timeout
    pub read_timeout: Duration,
    /// Write timeout
    pub write_timeout: Duration,
    /// Keep-alive interval
    pub keep_alive: Option<Duration>,
    /// Reconnect on disconnect
    pub auto_reconnect: bool,
    /// Maximum reconnect attempts
    pub max_reconnect_attempts: usize,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(10),
            keep_alive: Some(Duration::from_secs(60)),
            auto_reconnect: true,
            max_reconnect_attempts: 3,
        }
    }
}

impl ClientConfig {
    /// Create a new configuration
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set connection timeout
    #[must_use = "builder methods return a new value and do not modify the original"]
    pub fn with_connect_timeout(mut self, timeout: Duration) -> Self {
        self.connect_timeout = timeout;
        self
    }

    /// Set read timeout
    #[must_use = "builder methods return a new value and do not modify the original"]
    pub fn with_read_timeout(mut self, timeout: Duration) -> Self {
        self.read_timeout = timeout;
        self
    }

    /// Set write timeout
    #[must_use = "builder methods return a new value and do not modify the original"]
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    /// Set keep-alive interval
    #[must_use = "builder methods return a new value and do not modify the original"]
    pub fn with_keep_alive(mut self, interval: Option<Duration>) -> Self {
        self.keep_alive = interval;
        self
    }

    /// Set auto-reconnect
    #[must_use = "builder methods return a new value and do not modify the original"]
    pub fn with_auto_reconnect(mut self, enabled: bool) -> Self {
        self.auto_reconnect = enabled;
        self
    }
}

/// Client state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientState {
    Disconnected,
    Connecting,
    Connected,
    Reconnecting,
}

/// TCP client
pub struct TcpClient {
    config: ClientConfig,
    address: String,
    state: Arc<RwLock<ClientState>>,
    stream: Arc<Mutex<Option<TcpStream>>>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl TcpClient {
    /// Create a new TCP client
    #[must_use]
    pub fn new(address: impl Into<String>) -> Self {
        Self::with_config(address, ClientConfig::default())
    }

    /// Create a new TCP client with custom configuration
    #[must_use]
    pub fn with_config(address: impl Into<String>, config: ClientConfig) -> Self {
        Self {
            config,
            address: address.into(),
            state: Arc::new(RwLock::new(ClientState::Disconnected)),
            stream: Arc::new(Mutex::new(None)),
            shutdown_tx: None,
        }
    }

    /// Connect to the server
    pub async fn connect(&mut self) -> Result<()> {
        self.connect_internal(false).await
    }

    /// Internal connection implementation with reconnection support
    async fn connect_internal(&mut self, is_reconnect: bool) -> Result<()> {
        // Check current state
        {
            let state = self.state.read();
            if !is_reconnect
                && (*state == ClientState::Connected || *state == ClientState::Connecting)
            {
                return Err(NetworkError::invalid_state(
                    "Client is already connected or connecting",
                ));
            }
        }

        // Update state
        *self.state.write() = if is_reconnect {
            ClientState::Reconnecting
        } else {
            ClientState::Connecting
        };

        // Parse address
        let addr: SocketAddr = self
            .address
            .parse()
            .map_err(|e| NetworkError::invalid_address(format!("Invalid address: {}", e)))?;

        // Connect with timeout
        let stream = timeout(self.config.connect_timeout, TcpStream::connect(addr))
            .await
            .map_err(|_| NetworkError::timeout("Connection timed out"))?
            .map_err(|e| NetworkError::connection(format!("Failed to connect: {}", e)))?;

        // Set TCP options
        if let Some(keep_alive) = self.config.keep_alive {
            // Convert tokio stream to std socket for socket2 operations
            // Note: tokio TcpStream is always nonblocking, but std conversion may reset flags
            let std_stream = stream.into_std().map_err(|e| {
                NetworkError::other(format!("Failed to convert tokio stream to std: {}", e))
            })?;
            let socket = socket2::Socket::from(std_stream);

            // Configure TCP keep-alive
            let keep_alive = socket2::TcpKeepalive::new().with_time(keep_alive);
            socket
                .set_tcp_keepalive(&keep_alive)
                .map_err(|e| NetworkError::other(format!("Failed to set keep-alive: {}", e)))?;

            // CRITICAL: Restore nonblocking mode after socket2 operations
            // - tokio requires nonblocking sockets for async I/O
            // - socket2 operations may clear the nonblocking flag
            // - Must be set BEFORE converting back to tokio TcpStream
            socket.set_nonblocking(true).map_err(|e| {
                NetworkError::other(format!(
                    "Failed to restore nonblocking mode after keep-alive config: {}",
                    e
                ))
            })?;

            // Convert back to tokio stream (requires nonblocking socket)
            let stream = TcpStream::from_std(socket.into()).map_err(|e| {
                NetworkError::other(format!("Failed to convert std socket back to tokio: {}", e))
            })?;

            *self.stream.lock().await = Some(stream);
        } else {
            *self.stream.lock().await = Some(stream);
        }

        // Update state
        *self.state.write() = ClientState::Connected;

        Ok(())
    }

    /// Disconnect from the server
    pub async fn disconnect(&mut self) -> Result<()> {
        // Send shutdown signal
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }

        // Close stream
        if let Some(mut stream) = self.stream.lock().await.take() {
            let _ = stream.shutdown().await;
        }

        // Update state
        *self.state.write() = ClientState::Disconnected;

        Ok(())
    }

    /// Send a message with backpressure awareness
    ///
    /// This method waits for the socket to be writable before sending,
    /// providing natural backpressure when the send buffer is full.
    ///
    /// # Backpressure Handling
    ///
    /// - Waits for socket to be ready for writing (via `writable()`)
    /// - Prevents sending when TCP send buffer is full
    /// - Applies write timeout to both ready check and actual write
    ///
    /// Use `try_send()` if you need immediate failure on backpressure.
    pub async fn send(&self, message: &Message) -> Result<()> {
        // Check state
        {
            let state = self.state.read();
            if *state != ClientState::Connected {
                return Err(NetworkError::invalid_state("Client is not connected"));
            }
        }

        // Encode message
        let data = message.encode()?;

        // Get stream
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard
            .as_mut()
            .ok_or_else(|| NetworkError::invalid_state("No active connection"))?;

        // Wait for socket to be writable (backpressure handling)
        // This prevents overwhelming the TCP send buffer
        timeout(self.config.write_timeout, stream.writable())
            .await
            .map_err(|_| NetworkError::timeout("Socket not ready for writing (backpressure)"))?
            .map_err(|e| NetworkError::connection(format!("Failed to check writability: {}", e)))?;

        // Write with timeout
        timeout(self.config.write_timeout, stream.write_all(&data))
            .await
            .map_err(|_| NetworkError::timeout("Write timed out"))?
            .map_err(|e| NetworkError::connection(format!("Failed to write: {}", e)))?;

        Ok(())
    }

    /// Try to send a message without blocking on backpressure
    ///
    /// Returns immediately with an error if the socket is not ready for writing,
    /// providing explicit backpressure feedback to the caller.
    ///
    /// # Backpressure Detection
    ///
    /// Unlike `send()` which waits for writability, this method:
    /// - Checks if socket is immediately writable
    /// - Returns error if send buffer is full
    /// - Allows caller to handle backpressure explicitly
    ///
    /// # Use Cases
    ///
    /// - Non-blocking network operations
    /// - Load shedding when overwhelmed
    /// - Explicit flow control in application layer
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rust_network_system::core::{TcpClient, Message};
    /// # async fn example(client: &TcpClient, msg: &Message) -> Result<(), Box<dyn std::error::Error>> {
    /// match client.try_send(msg).await {
    ///     Ok(()) => println!("Sent successfully"),
    ///     Err(e) if e.to_string().contains("backpressure") => {
    ///         println!("Send buffer full, will retry later");
    ///         // Handle backpressure: queue for later, drop message, etc.
    ///     }
    ///     Err(e) => return Err(e.into()),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn try_send(&self, message: &Message) -> Result<()> {
        // Check state
        {
            let state = self.state.read();
            if *state != ClientState::Connected {
                return Err(NetworkError::invalid_state("Client is not connected"));
            }
        }

        // Encode message
        let data = message.encode()?;

        // Get stream
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard
            .as_mut()
            .ok_or_else(|| NetworkError::invalid_state("No active connection"))?;

        // Check if immediately writable (non-blocking backpressure check)
        match stream.try_write(&data) {
            Ok(n) if n == data.len() => {
                // Full message written immediately
                Ok(())
            }
            Ok(n) => {
                // Partial write - send buffer is filling up
                // Write remaining data with timeout
                timeout(self.config.write_timeout, stream.write_all(&data[n..]))
                    .await
                    .map_err(|_| NetworkError::timeout("Write timed out after partial write"))?
                    .map_err(|e| {
                        NetworkError::connection(format!("Failed to complete write: {}", e))
                    })?;
                Ok(())
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Socket not ready - backpressure detected
                Err(NetworkError::other(
                    "Backpressure: send buffer full, cannot write without blocking",
                ))
            }
            Err(e) => {
                // Other I/O error
                Err(NetworkError::connection(format!("Failed to write: {}", e)))
            }
        }
    }

    /// Receive a message
    pub async fn receive(&self) -> Result<Message> {
        // Check state
        {
            let state = self.state.read();
            if *state != ClientState::Connected {
                return Err(NetworkError::invalid_state("Client is not connected"));
            }
        }

        // Get stream
        let mut stream_guard = self.stream.lock().await;
        let stream = stream_guard
            .as_mut()
            .ok_or_else(|| NetworkError::invalid_state("No active connection"))?;

        // Maximum buffer size to prevent infinite loop on malformed data
        const MAX_BUFFER_SIZE: usize = crate::core::message::MAX_MESSAGE_SIZE + 1024;
        let mut buf = BytesMut::with_capacity(4096);

        // Total message assembly timeout (prevents SlowLoris attacks)
        // This is separate from per-read timeout to limit total time for complete message
        let assembly_timeout = self.config.read_timeout * 3; // 3x read timeout for full message
        let assembly_start = tokio::time::Instant::now();

        loop {
            // Check total time spent assembling message
            if assembly_start.elapsed() >= assembly_timeout {
                return Err(NetworkError::timeout(
                    "Message assembly timeout: took too long to receive complete message",
                ));
            }

            // Try to decode existing data
            if let Some(message) = Message::decode(&mut buf)? {
                return Ok(message);
            }

            // Check buffer size to prevent infinite loop on malformed data
            // If buffer exceeds max size and decode still returns None, data is malformed
            if buf.len() >= MAX_BUFFER_SIZE {
                return Err(NetworkError::connection(
                    "Malformed message: buffer exceeded maximum size without complete message",
                ));
            }

            // Read more data with per-read timeout
            let n = timeout(self.config.read_timeout, stream.read_buf(&mut buf))
                .await
                .map_err(|_| NetworkError::timeout("Read timed out"))?
                .map_err(|e| NetworkError::connection(format!("Failed to read: {}", e)))?;

            if n == 0 {
                return Err(NetworkError::connection("Connection closed by peer"));
            }
        }
    }

    /// Check if client is connected
    #[must_use]
    pub fn is_connected(&self) -> bool {
        *self.state.read() == ClientState::Connected
    }

    /// Get the current state
    #[must_use]
    pub fn state(&self) -> ClientState {
        *self.state.read()
    }

    /// Get the remote address
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// Attempt to reconnect to the server with exponential backoff
    ///
    /// # Reconnection Strategy
    ///
    /// - **Exponential backoff**: Delay doubles after each failed attempt
    /// - **Jitter**: Random delay added to prevent thundering herd
    /// - **Max attempts**: Respects `max_reconnect_attempts` from config
    /// - **Initial delay**: 100ms, grows to max 30s
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rust_network_system::core::{TcpClient, ClientConfig};
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut client = TcpClient::with_config(
    ///     "127.0.0.1:8080",
    ///     ClientConfig::new().with_auto_reconnect(true)
    /// );
    ///
    /// // Initial connection
    /// client.connect().await?;
    ///
    /// // If connection lost, manually reconnect
    /// if !client.is_connected() {
    ///     match client.reconnect().await {
    ///         Ok(()) => println!("Reconnected successfully"),
    ///         Err(e) => eprintln!("Failed to reconnect: {}", e),
    ///     }
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn reconnect(&mut self) -> Result<()> {
        use rand::Rng;

        let max_attempts = self.config.max_reconnect_attempts;
        let mut attempt = 0;
        let mut delay_ms = 100u64; // Start with 100ms
        const MAX_DELAY_MS: u64 = 30_000; // Cap at 30 seconds

        while attempt < max_attempts {
            attempt += 1;

            tracing::info!(
                "Reconnection attempt {}/{} to {}",
                attempt,
                max_attempts,
                self.address
            );

            // Try to connect
            match self.connect_internal(true).await {
                Ok(()) => {
                    tracing::info!("Successfully reconnected to {}", self.address);
                    return Ok(());
                }
                Err(e) => {
                    tracing::warn!(
                        "Reconnection attempt {}/{} failed: {}",
                        attempt,
                        max_attempts,
                        e
                    );

                    // Last attempt failed
                    if attempt >= max_attempts {
                        *self.state.write() = ClientState::Disconnected;
                        return Err(NetworkError::connection(format!(
                            "Failed to reconnect after {} attempts",
                            max_attempts
                        )));
                    }

                    // Exponential backoff with jitter
                    let jitter = rand::thread_rng().gen_range(0..100);
                    let sleep_duration = Duration::from_millis(delay_ms + jitter);

                    tracing::debug!(
                        "Waiting {:?} before next reconnection attempt",
                        sleep_duration
                    );

                    tokio::time::sleep(sleep_duration).await;

                    // Double the delay for next attempt, capped at MAX_DELAY_MS
                    delay_ms = (delay_ms * 2).min(MAX_DELAY_MS);
                }
            }
        }

        *self.state.write() = ClientState::Disconnected;
        Err(NetworkError::connection(
            "Max reconnection attempts reached",
        ))
    }

    /// Attempt to send with automatic reconnection if enabled
    ///
    /// This is a wrapper around `send()` that automatically attempts to
    /// reconnect if the connection is lost and auto_reconnect is enabled.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rust_network_system::core::{TcpClient, ClientConfig, Message};
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut client = TcpClient::with_config(
    ///     "127.0.0.1:8080",
    ///     ClientConfig::new().with_auto_reconnect(true)
    /// );
    /// client.connect().await?;
    ///
    /// let msg = Message::new("Hello");
    /// // Automatically reconnects if connection is lost
    /// client.send_with_reconnect(&msg).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_with_reconnect(&mut self, message: &Message) -> Result<()> {
        match self.send(message).await {
            Ok(()) => Ok(()),
            Err(e) => {
                // Check if error is connection-related and auto-reconnect is enabled
                if self.config.auto_reconnect && self.is_connection_error(&e) {
                    tracing::warn!("Connection lost during send, attempting to reconnect");
                    self.reconnect().await?;
                    // Retry send after reconnection
                    self.send(message).await
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Attempt to receive with automatic reconnection if enabled
    ///
    /// This is a wrapper around `receive()` that automatically attempts to
    /// reconnect if the connection is lost and auto_reconnect is enabled.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rust_network_system::core::{TcpClient, ClientConfig};
    /// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
    /// let mut client = TcpClient::with_config(
    ///     "127.0.0.1:8080",
    ///     ClientConfig::new().with_auto_reconnect(true)
    /// );
    /// client.connect().await?;
    ///
    /// // Automatically reconnects if connection is lost
    /// let msg = client.receive_with_reconnect().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn receive_with_reconnect(&mut self) -> Result<Message> {
        match self.receive().await {
            Ok(msg) => Ok(msg),
            Err(e) => {
                // Check if error is connection-related and auto-reconnect is enabled
                if self.config.auto_reconnect && self.is_connection_error(&e) {
                    tracing::warn!("Connection lost during receive, attempting to reconnect");
                    self.reconnect().await?;
                    // Retry receive after reconnection
                    self.receive().await
                } else {
                    Err(e)
                }
            }
        }
    }

    /// Check if an error is connection-related (indicating a lost connection)
    fn is_connection_error(&self, error: &NetworkError) -> bool {
        matches!(
            error,
            NetworkError::Connection(_) | NetworkError::InvalidState(_)
        )
    }
}

/// No-op cleanup when TcpClient is dropped
///
/// # Design Decision
///
/// Unlike `TcpServer`, `TcpClient` does NOT perform cleanup in Drop because:
/// 1. **Async limitations**: Drop cannot await, so graceful disconnect is impossible
/// 2. **Simple resource model**: Client has fewer resources than server (no task spawning)
/// 3. **Explicit lifecycle**: Encourages explicit connection management
///
/// # Behavior
///
/// When a `TcpClient` is dropped without calling `disconnect()`:
/// - **No explicit cleanup** is performed in the Drop implementation
/// - **Stream cleanup** is delegated to `TcpStream`'s Drop implementation
/// - **TCP FIN** is sent by the OS when the socket is closed
/// - **State remains unchanged** (stays in Connected/Connecting state)
///
/// # Resource Cleanup
///
/// The underlying `TcpStream` is wrapped in `Arc<Mutex<Option<TcpStream>>>`:
/// - When the last Arc reference is dropped, the Mutex is dropped
/// - When the Mutex is dropped, the Option is dropped
/// - When the Option is dropped, the TcpStream is dropped
/// - When TcpStream is dropped, the OS closes the TCP connection
///
/// This means the connection WILL be closed, but **not gracefully**.
///
/// # Graceful vs Ungraceful Shutdown
///
/// **Graceful shutdown** (via `disconnect()`):
/// - Sends any pending data before closing
/// - Properly shuts down the write half of the connection
/// - Updates client state to Disconnected
/// - Allows the peer to detect clean disconnect
///
/// **Ungraceful shutdown** (via Drop):
/// - OS sends TCP RST or FIN immediately when socket is closed
/// - Pending data may be lost
/// - Peer may see connection as aborted rather than cleanly closed
/// - Client state remains in Connected state (no cleanup)
///
/// # Best Practices
///
/// ```no_run
/// use rust_network_system::core::{TcpClient, ClientConfig};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let mut client = TcpClient::with_config("127.0.0.1:8080", ClientConfig::default());
/// client.connect().await?;
///
/// // Use the client...
/// // let msg = Message::new(/* ... */);
/// // client.send(&msg).await?;
///
/// // ✅ CORRECT: Graceful disconnect before drop
/// client.disconnect().await?;
/// // Drop occurs here with no cleanup needed
///
/// // ❌ LESS IDEAL: Let client drop while connected
/// // Connection will close, but not gracefully
/// # Ok(())
/// # }
/// ```
///
/// # When Ungraceful Shutdown is Acceptable
///
/// Skipping `disconnect()` may be acceptable when:
/// - The peer expects abrupt disconnects (e.g., fire-and-forget protocols)
/// - The connection is already broken (network failure detected)
/// - The application is terminating anyway (process exit)
/// - Performance is critical and clean shutdown is not required
///
/// # Comparison with TcpServer
///
/// **TcpServer** performs emergency cleanup in Drop because:
/// - It spawns tasks that need to be aborted
/// - It manages multiple sessions that should be closed
/// - It has complex state that benefits from cleanup logging
///
/// **TcpClient** does NOT perform cleanup in Drop because:
/// - It has simpler resource model (no spawned tasks)
/// - Stream cleanup is sufficient for most use cases
/// - Explicit lifecycle management is clearer for single connections
impl Drop for TcpClient {
    fn drop(&mut self) {
        // Cleanup handled by explicit disconnect() call
        // Note: Cannot use async in Drop, so cleanup must be done explicitly
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_client_config() {
        let config = ClientConfig::new()
            .with_connect_timeout(Duration::from_secs(5))
            .with_read_timeout(Duration::from_secs(10))
            .with_auto_reconnect(false);

        assert_eq!(config.connect_timeout, Duration::from_secs(5));
        assert_eq!(config.read_timeout, Duration::from_secs(10));
        assert!(!config.auto_reconnect);
    }

    #[test]
    fn test_client_creation() {
        let client = TcpClient::new("127.0.0.1:8080");
        assert!(!client.is_connected());
        assert_eq!(client.address(), "127.0.0.1:8080");
    }
}
