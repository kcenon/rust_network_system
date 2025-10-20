//! TCP server implementation

use crate::core::message::Message;
use crate::error::{NetworkError, Result};
use crate::session::{Session, SessionHandler};
use parking_lot::RwLock;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

/// Server configuration
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Bind address
    pub bind_address: String,
    /// Maximum concurrent connections
    pub max_connections: usize,
    /// Read timeout
    pub read_timeout: Duration,
    /// Write timeout
    pub write_timeout: Duration,
    /// Idle timeout (session automatically closed if no activity)
    pub idle_timeout: Duration,
    /// Keep-alive interval
    pub keep_alive: Option<Duration>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind_address: "127.0.0.1:8080".to_string(),
            max_connections: 1000,
            read_timeout: Duration::from_secs(30),
            write_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(300), // 5 minutes default
            keep_alive: Some(Duration::from_secs(60)),
        }
    }
}

impl ServerConfig {
    /// Create a new configuration
    #[must_use]
    pub fn new<S: Into<String>>(bind_address: S) -> Self {
        Self {
            bind_address: bind_address.into(),
            ..Default::default()
        }
    }

    /// Set maximum connections
    #[must_use = "builder methods return a new value and do not modify the original"]
    pub fn with_max_connections(mut self, max: usize) -> Self {
        self.max_connections = max;
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

    /// Set idle timeout (session closed after this period of inactivity)
    #[must_use = "builder methods return a new value and do not modify the original"]
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Set keep-alive interval
    #[must_use = "builder methods return a new value and do not modify the original"]
    pub fn with_keep_alive(mut self, interval: Option<Duration>) -> Self {
        self.keep_alive = interval;
        self
    }
}

/// TCP server
pub struct TcpServer {
    config: ServerConfig,
    running: Arc<AtomicBool>,
    connection_count: Arc<AtomicU64>,
    /// Semaphore for atomic connection limiting (prevents race conditions)
    connection_semaphore: Arc<Semaphore>,
    sessions: Arc<RwLock<Vec<Arc<Session>>>>,
    session_tasks: Arc<RwLock<Vec<JoinHandle<()>>>>,
    server_task: Option<JoinHandle<()>>,
    cleanup_task: Option<JoinHandle<()>>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl TcpServer {
    /// Create a new TCP server
    #[must_use]
    pub fn new(bind_address: impl Into<String>) -> Self {
        Self::with_config(ServerConfig::new(bind_address))
    }

    /// Create a new TCP server with custom configuration
    #[must_use]
    pub fn with_config(config: ServerConfig) -> Self {
        let connection_semaphore = Arc::new(Semaphore::new(config.max_connections));

        Self {
            config,
            running: Arc::new(AtomicBool::new(false)),
            connection_count: Arc::new(AtomicU64::new(0)),
            connection_semaphore,
            sessions: Arc::new(RwLock::new(Vec::new())),
            session_tasks: Arc::new(RwLock::new(Vec::new())),
            server_task: None,
            cleanup_task: None,
            shutdown_tx: None,
        }
    }

    /// Start the server
    pub async fn start<H>(&mut self, handler: Arc<H>) -> Result<()>
    where
        H: SessionHandler + Send + Sync + 'static,
    {
        if self.running.load(Ordering::Acquire) {
            return Err(NetworkError::invalid_state("Server is already running"));
        }

        // Parse bind address
        let addr: SocketAddr = self
            .config
            .bind_address
            .parse()
            .map_err(|e| NetworkError::invalid_address(format!("Invalid address: {}", e)))?;

        // Bind listener
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| NetworkError::connection(format!("Failed to bind: {}", e)))?;

        // Create shutdown channel
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
        self.shutdown_tx = Some(shutdown_tx);

        // Clone shared state
        let running = Arc::clone(&self.running);
        let connection_count = Arc::clone(&self.connection_count);
        let connection_semaphore = Arc::clone(&self.connection_semaphore);
        let sessions = Arc::clone(&self.sessions);
        let session_tasks = Arc::clone(&self.session_tasks);
        let max_connections = self.config.max_connections;
        let read_timeout = self.config.read_timeout;
        let write_timeout = self.config.write_timeout;
        let idle_timeout = self.config.idle_timeout;
        let keep_alive = self.config.keep_alive;

        // Set running flag before spawning task
        // This ensures is_running() returns true immediately after start() completes
        self.running.store(true, Ordering::Release);

        // Start server task
        let server_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    // Accept new connection
                    result = listener.accept() => {
                        match result {
                            Ok((stream, peer_addr)) => {
                                // Try to acquire a connection permit from semaphore
                                // This is atomic and prevents race conditions in connection limiting
                                let permit = match connection_semaphore.clone().try_acquire_owned() {
                                    Ok(permit) => {
                                        // Successfully acquired permit - connection allowed
                                        connection_count.fetch_add(1, Ordering::Relaxed);
                                        permit
                                    }
                                    Err(_) => {
                                        // No permits available - connection limit reached
                                        tracing::warn!(
                                            "Connection limit reached ({}), rejecting {}",
                                            max_connections,
                                            peer_addr
                                        );
                                        // Close the connection immediately
                                        drop(stream);
                                        continue;
                                    }
                                };

                                // Set TCP options and handle connection
                                let mut stream_to_use = Some(stream);

                                if let Some(keep_alive_duration) = keep_alive {
                                    if let Some(s) = stream_to_use.take() {
                                        match s.into_std() {
                                            Ok(std_stream) => {
                                                let socket = socket2::Socket::from(std_stream);
                                                let keep_alive = socket2::TcpKeepalive::new()
                                                    .with_time(keep_alive_duration);

                                                if let Err(e) = socket.set_tcp_keepalive(&keep_alive) {
                                                    tracing::warn!("Failed to set keep-alive: {}", e);
                                                }

                                                // Set non-blocking mode before converting to tokio stream
                                                if let Err(e) = socket.set_nonblocking(true) {
                                                    tracing::warn!("Failed to set non-blocking mode: {}", e);
                                                }

                                                match TcpStream::from_std(socket.into()) {
                                                    Ok(tokio_stream) => {
                                                        stream_to_use = Some(tokio_stream);
                                                    }
                                                    Err(e) => {
                                                        tracing::error!("Failed to convert socket back to tokio stream: {}", e);
                                                        connection_count.fetch_sub(1, Ordering::Release);
                                                        continue;
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::error!("Failed to convert to std stream: {}", e);
                                                connection_count.fetch_sub(1, Ordering::Release);
                                                continue;
                                            }
                                        }
                                    }
                                }

                                if let Some(final_stream) = stream_to_use {
                                    let task_handle = Self::handle_connection(
                                        final_stream,
                                        peer_addr,
                                        Arc::clone(&handler),
                                        Arc::clone(&sessions),
                                        Arc::clone(&connection_count),
                                        read_timeout,
                                        write_timeout,
                                        idle_timeout,
                                        permit, // Pass permit - will be released on drop
                                    );

                                    // Track the session task to prevent resource leaks
                                    session_tasks.write().push(task_handle);

                                    // Cleanup finished tasks to prevent unbounded growth
                                    session_tasks.write().retain(|task| !task.is_finished());
                                } else {
                                    // Should never happen, but release permit and decrement counter
                                    drop(permit);
                                    connection_count.fetch_sub(1, Ordering::Release);
                                }
                            }
                            Err(e) => {
                                tracing::error!("Failed to accept connection: {}", e);
                            }
                        }
                    }
                    // Shutdown signal
                    _ = shutdown_rx.recv() => {
                        tracing::info!("Server received shutdown signal");
                        break;
                    }
                }
            }

            running.store(false, Ordering::Release);
        });

        self.server_task = Some(server_task);

        // Start periodic cleanup task for idle sessions
        let cleanup_sessions = Arc::clone(&self.sessions);
        let cleanup_running = Arc::clone(&self.running);
        let cleanup_interval = self.config.idle_timeout / 2; // Check at half the idle timeout

        let cleanup_task = tokio::spawn(async move {
            while cleanup_running.load(Ordering::Acquire) {
                tokio::time::sleep(cleanup_interval).await;

                // Find and close idle sessions
                let idle_sessions: Vec<Arc<Session>> = {
                    let sessions = cleanup_sessions.read();
                    sessions
                        .iter()
                        .filter(|session| session.is_idle())
                        .cloned()
                        .collect()
                };

                for session in idle_sessions {
                    tracing::info!(
                        "Closing idle session {} (idle for {:?})",
                        session.id(),
                        session.idle_time()
                    );
                    let _ = session.close().await;
                }

                // Cleanup finished tasks periodically
                // This prevents unbounded growth in long-running servers
                // where connections are accepted infrequently
            }
        });

        self.cleanup_task = Some(cleanup_task);

        Ok(())
    }

    /// Stop the server
    pub async fn stop(&mut self) -> Result<()> {
        if !self.running.load(Ordering::Acquire) {
            return Ok(());
        }

        // Signal shutdown (this will stop both server and cleanup tasks)
        self.running.store(false, Ordering::Release);

        // Send shutdown signal
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(()).await;
        }

        // Wait for server task
        if let Some(task) = self.server_task.take() {
            let _ = task.await;
        }

        // Wait for cleanup task
        if let Some(task) = self.cleanup_task.take() {
            let _ = task.await;
        }

        // Close all sessions
        let sessions = self.sessions.read().clone();
        for session in sessions {
            let _ = session.close().await;
        }
        self.sessions.write().clear();

        // Abort all remaining session tasks to prevent resource leaks
        let tasks = {
            let mut tasks = self.session_tasks.write();
            std::mem::take(&mut *tasks)
        };

        for task in tasks {
            if !task.is_finished() {
                task.abort();
            }
        }

        self.running.store(false, Ordering::Release);

        Ok(())
    }

    /// Handle a new connection
    ///
    /// Returns a JoinHandle for the session task to enable proper resource cleanup
    ///
    /// The semaphore permit is held for the lifetime of the connection and automatically
    /// released when the task completes, ensuring accurate connection limiting.
    #[allow(clippy::too_many_arguments)]
    fn handle_connection<H>(
        stream: tokio::net::TcpStream,
        peer_addr: SocketAddr,
        handler: Arc<H>,
        sessions: Arc<RwLock<Vec<Arc<Session>>>>,
        connection_count: Arc<AtomicU64>,
        read_timeout: Duration,
        write_timeout: Duration,
        idle_timeout: Duration,
        _permit: OwnedSemaphorePermit, // Held until connection closes
    ) -> JoinHandle<()>
    where
        H: SessionHandler + Send + Sync + 'static,
    {
        tokio::spawn(async move {
            let session = Arc::new(Session::new(
                stream,
                peer_addr,
                read_timeout,
                write_timeout,
                idle_timeout,
            ));

            // Add to session list
            sessions.write().push(Arc::clone(&session));

            // Handle session
            handler.on_connected(&session).await;

            // Session loop
            loop {
                match session.receive().await {
                    Ok(message) => {
                        handler.on_message(&session, message).await;
                    }
                    Err(e) => {
                        tracing::debug!("Session error: {}", e);
                        break;
                    }
                }
            }

            // Cleanup
            handler.on_disconnected(&session).await;

            // Remove from session list
            sessions.write().retain(|s| !Arc::ptr_eq(s, &session));

            // Decrement connection count
            connection_count.fetch_sub(1, Ordering::Release);

            // Permit is automatically released here when _permit is dropped
        })
    }

    /// Check if server is running
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::Acquire)
    }

    /// Get active connection count
    #[must_use]
    pub fn connection_count(&self) -> u64 {
        self.connection_count.load(Ordering::Relaxed)
    }

    /// Get active sessions
    pub fn sessions(&self) -> Vec<Arc<Session>> {
        self.sessions.read().clone()
    }

    /// Broadcast a message to all sessions
    pub async fn broadcast(&self, message: &Message) -> Result<()> {
        let sessions = self.sessions.read().clone();
        for session in sessions {
            let _ = session.send(message).await;
        }
        Ok(())
    }
}

/// Emergency cleanup when TcpServer is dropped
///
/// # Behavior
///
/// When a `TcpServer` is dropped, it performs emergency cleanup if the server
/// is still running (i.e., `stop()` was not called before drop):
///
/// 1. **Logs warning** about emergency cleanup to help detect improper usage
/// 2. **Sends shutdown signal** via channel (best effort, non-blocking)
/// 3. **Aborts server task** to stop accepting new connections
/// 4. **Aborts cleanup task** to stop idle session checks
/// 5. **Aborts all session tasks** to prevent resource leaks
/// 6. **Marks sessions inactive** without awaiting (graceful close not possible in Drop)
/// 7. **Clears session list** to release Arc references
/// 8. **Sets running flag to false** with Release ordering
///
/// # Async Limitations
///
/// **IMPORTANT**: Drop cannot be async, so cleanup is best-effort:
/// - **Cannot await**: All async operations are aborted rather than gracefully completed
/// - **No graceful shutdown**: Sessions are forcibly terminated, not gracefully closed
/// - **Resource cleanup**: Relies on underlying Drop implementations (TcpStream, etc.)
///
/// For graceful shutdown, **always call `stop().await` explicitly** before dropping.
///
/// # Memory Ordering
///
/// Uses `Ordering::Acquire` to read the running flag and `Ordering::Release` to set it:
/// - **Acquire**: Ensures visibility of all server state modifications from other threads
/// - **Release**: Makes the shutdown visible to any threads still referencing the server
///
/// # Thread Safety
///
/// This Drop implementation is:
/// - **Panic-safe**: Always executes, even during stack unwinding
/// - **Thread-safe**: Uses atomic operations and locks for state modifications
/// - **Non-blocking**: All cleanup operations are immediate (no awaits)
///
/// # Resource Leak Prevention
///
/// The emergency cleanup prevents several resource leaks:
/// - **Task leaks**: Aborts all spawned tasks (server, cleanup, sessions)
/// - **Connection leaks**: Session aborts trigger TCP stream cleanup
/// - **Memory leaks**: Clears session list to break Arc cycles
///
/// # Best Practices
///
/// ```no_run
/// use rust_network_system::core::{TcpServer, ServerConfig};
/// use std::sync::Arc;
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let mut server = TcpServer::with_config(ServerConfig::default());
/// // let handler = Arc::new(MyHandler);
/// // server.start(handler).await?;
///
/// // ✅ CORRECT: Graceful shutdown before drop
/// server.stop().await?;
/// // Drop occurs here with minimal cleanup needed
///
/// // ❌ INCORRECT: Don't let server drop while running
/// // This triggers emergency cleanup and logs a warning
/// # Ok(())
/// # }
/// ```
///
/// # Warning Detection
///
/// If you see this log message, review your shutdown logic:
/// ```text
/// WARN TcpServer dropped while still running - performing emergency cleanup
/// ```
impl Drop for TcpServer {
    fn drop(&mut self) {
        // Best-effort cleanup when dropped without explicit stop()
        if self.running.load(Ordering::Acquire) {
            tracing::warn!("TcpServer dropped while still running - performing emergency cleanup");

            // Send shutdown signal (best effort)
            if let Some(tx) = self.shutdown_tx.take() {
                // Can't await in Drop, use blocking send
                let _ = tx.try_send(());
            }

            // Abort server task if still running
            if let Some(task) = self.server_task.take() {
                task.abort();
            }

            // Abort cleanup task if still running
            if let Some(task) = self.cleanup_task.take() {
                task.abort();
            }

            // Abort all session tasks to prevent resource leaks
            let tasks = {
                let mut tasks = self.session_tasks.write();
                std::mem::take(&mut *tasks)
            };

            for task in tasks {
                if !task.is_finished() {
                    task.abort();
                }
            }

            // Close all sessions synchronously
            // Note: We can't await in Drop, so we do best-effort cleanup
            let sessions = self.sessions.read().clone();
            for session in sessions {
                // Mark sessions as inactive without awaiting close()
                // The session's own Drop will handle stream cleanup
                session.set_inactive();
            }
            self.sessions.write().clear();

            self.running.store(false, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config() {
        let config = ServerConfig::new("0.0.0.0:9000")
            .with_max_connections(500)
            .with_read_timeout(Duration::from_secs(60));

        assert_eq!(config.bind_address, "0.0.0.0:9000");
        assert_eq!(config.max_connections, 500);
        assert_eq!(config.read_timeout, Duration::from_secs(60));
    }

    #[test]
    fn test_server_creation() {
        let server = TcpServer::new("127.0.0.1:8080");
        assert!(!server.is_running());
        assert_eq!(server.connection_count(), 0);
    }
}
