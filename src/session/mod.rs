//! Session management

use crate::core::message::Message;
use crate::error::{NetworkError, Result};
use async_trait::async_trait;
use bytes::BytesMut;
use parking_lot::RwLock;
use rand::rngs::OsRng;
use rand::Rng;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::sync::Mutex;
use tokio::time::timeout;

/// Generate a cryptographically secure session ID
///
/// Uses 128-bit random ID from OS RNG to prevent session hijacking through ID prediction.
/// The probability of collision is astronomically low (1 in 2^128).
fn generate_session_id() -> u128 {
    OsRng.gen()
}

/// Session state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Active,
    Closing,
    Closed,
}

/// Network session representing a client connection
///
/// # Concurrency Design
///
/// The TCP stream is split into independent read and write halves to prevent
/// deadlock between concurrent send() and receive() operations. Each half is
/// protected by its own Mutex, allowing:
/// - One task to read while another writes
/// - No blocking between send() and receive()
/// - Full-duplex communication without lock contention
pub struct Session {
    id: u128,
    // Split stream for independent read/write access
    read_half: Arc<Mutex<Option<OwnedReadHalf>>>,
    write_half: Arc<Mutex<Option<OwnedWriteHalf>>>,
    peer_addr: SocketAddr,
    state: Arc<RwLock<SessionState>>,
    read_timeout: Duration,
    write_timeout: Duration,
    idle_timeout: Duration,
    created_at: Instant,
    last_activity: Arc<RwLock<Instant>>,
    active: Arc<AtomicBool>,
    // Read buffer persists across receive() calls to handle multiple messages in single read
    read_buffer: Arc<Mutex<BytesMut>>,
}

impl Session {
    /// Create a new session with split read/write halves for concurrent access
    #[must_use]
    pub fn new(
        stream: TcpStream,
        peer_addr: SocketAddr,
        read_timeout: Duration,
        write_timeout: Duration,
        idle_timeout: Duration,
    ) -> Self {
        let id = generate_session_id();
        let now = Instant::now();

        // Split stream into read and write halves for independent concurrent access
        // This prevents send() and receive() from blocking each other
        let (read_half, write_half) = stream.into_split();

        Self {
            id,
            read_half: Arc::new(Mutex::new(Some(read_half))),
            write_half: Arc::new(Mutex::new(Some(write_half))),
            peer_addr,
            state: Arc::new(RwLock::new(SessionState::Active)),
            read_timeout,
            write_timeout,
            idle_timeout,
            created_at: now,
            last_activity: Arc::new(RwLock::new(now)),
            active: Arc::new(AtomicBool::new(true)),
            read_buffer: Arc::new(Mutex::new(BytesMut::with_capacity(4096))),
        }
    }

    /// Get session ID (cryptographically secure 128-bit random ID)
    #[must_use]
    pub fn id(&self) -> u128 {
        self.id
    }

    /// Update last activity time (called on send/receive)
    fn update_activity(&self) {
        *self.last_activity.write() = Instant::now();
    }

    /// Check if session has exceeded idle timeout
    #[must_use]
    pub fn is_idle(&self) -> bool {
        self.last_activity.read().elapsed() > self.idle_timeout
    }

    /// Get time since last activity
    #[must_use]
    pub fn idle_time(&self) -> Duration {
        self.last_activity.read().elapsed()
    }

    /// Get peer address
    #[must_use]
    pub fn peer_addr(&self) -> SocketAddr {
        self.peer_addr
    }

    /// Get session state
    #[must_use]
    pub fn state(&self) -> SessionState {
        *self.state.read()
    }

    /// Check if session is active
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    /// Set session as inactive (synchronous, for use in Drop)
    pub fn set_inactive(&self) {
        self.active.store(false, Ordering::Release);
    }

    /// Get session uptime
    #[must_use]
    pub fn uptime(&self) -> Duration {
        self.created_at.elapsed()
    }

    /// Send a message (non-blocking with respect to concurrent receive())
    ///
    /// Uses independent write half, allowing send() and receive() to run concurrently
    /// without lock contention.
    pub async fn send(&self, message: &Message) -> Result<()> {
        // Check state
        if !self.is_active() {
            return Err(NetworkError::invalid_state("Session is not active"));
        }

        // Check idle timeout
        if self.is_idle() {
            return Err(NetworkError::timeout("Session idle timeout exceeded"));
        }

        // Encode message
        let data = message.encode()?;

        // Get write half (independent of read half - no blocking)
        let mut write_guard = self.write_half.lock().await;
        let write_half = write_guard
            .as_mut()
            .ok_or_else(|| NetworkError::invalid_state("Session write half is closed"))?;

        // Write with timeout
        timeout(self.write_timeout, write_half.write_all(&data))
            .await
            .map_err(|_| NetworkError::timeout("Write timed out"))?
            .map_err(|e| NetworkError::connection(format!("Failed to write: {}", e)))?;

        // Update activity timestamp on successful send
        self.update_activity();

        Ok(())
    }

    /// Receive a message (non-blocking with respect to concurrent send())
    ///
    /// Uses independent read half, allowing send() and receive() to run concurrently
    /// without lock contention. The read buffer persists across calls to handle cases
    /// where multiple messages arrive in a single TCP packet.
    pub async fn receive(&self) -> Result<Message> {
        // Check state
        if !self.is_active() {
            return Err(NetworkError::invalid_state("Session is not active"));
        }

        // Check idle timeout
        if self.is_idle() {
            return Err(NetworkError::timeout("Session idle timeout exceeded"));
        }

        // Get persistent read buffer (persists across calls to handle multiple messages)
        let mut buf_guard = self.read_buffer.lock().await;

        // Get read half (independent of write half - no blocking)
        let mut read_guard = self.read_half.lock().await;
        let read_half = read_guard
            .as_mut()
            .ok_or_else(|| NetworkError::invalid_state("Session read half is closed"))?;

        // Maximum buffer size to prevent infinite loop on malformed data
        const MAX_BUFFER_SIZE: usize = crate::core::message::MAX_MESSAGE_SIZE + 1024;

        // Total message assembly timeout (prevents SlowLoris attacks)
        // This is separate from per-read timeout to limit total time for complete message
        let assembly_timeout = self.read_timeout * 3; // 3x read timeout for full message
        let assembly_start = tokio::time::Instant::now();

        loop {
            // Check total time spent assembling message
            if assembly_start.elapsed() >= assembly_timeout {
                return Err(NetworkError::timeout(
                    "Message assembly timeout: took too long to receive complete message",
                ));
            }

            // Try to decode existing data (may contain message from previous read)
            if let Some(message) = Message::decode(&mut buf_guard)? {
                // Update activity timestamp on successful receive
                self.update_activity();
                return Ok(message);
            }

            // Check buffer size to prevent infinite loop on malformed data
            // If buffer exceeds max size and decode still returns None, data is malformed
            if buf_guard.len() >= MAX_BUFFER_SIZE {
                return Err(NetworkError::connection(
                    "Malformed message: buffer exceeded maximum size without complete message",
                ));
            }

            // Read more data with per-read timeout
            // Deref MutexGuard to get &mut BytesMut which implements BufMut
            let n = timeout(self.read_timeout, read_half.read_buf(&mut *buf_guard))
                .await
                .map_err(|_| NetworkError::timeout("Read timed out"))?
                .map_err(|e| NetworkError::connection(format!("Failed to read: {}", e)))?;

            if n == 0 {
                return Err(NetworkError::connection("Connection closed by peer"));
            }
        }
    }

    /// Close the session gracefully
    ///
    /// Closes both read and write halves of the TCP stream. The shutdown is best-effort
    /// and errors are ignored to ensure cleanup proceeds even if one half fails.
    pub async fn close(&self) -> Result<()> {
        // Update state
        *self.state.write() = SessionState::Closing;
        self.active.store(false, Ordering::Release);

        // Close write half (sends FIN to peer)
        if let Some(mut write_half) = self.write_half.lock().await.take() {
            // Best effort shutdown - ignore errors
            let _ = write_half.shutdown().await;
        }

        // Close read half (releases resources)
        // No explicit shutdown needed - drop is sufficient
        let _ = self.read_half.lock().await.take();

        // Update state
        *self.state.write() = SessionState::Closed;

        Ok(())
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("peer_addr", &self.peer_addr)
            .field("state", &self.state())
            .field("uptime", &self.uptime())
            .finish()
    }
}

/// Session event handler
#[async_trait]
pub trait SessionHandler {
    /// Called when a new session is connected
    async fn on_connected(&self, session: &Session);

    /// Called when a message is received
    async fn on_message(&self, session: &Session, message: Message);

    /// Called when a session is disconnected
    async fn on_disconnected(&self, session: &Session);
}

/// Default session handler (does nothing)
pub struct DefaultSessionHandler;

#[async_trait]
impl SessionHandler for DefaultSessionHandler {
    async fn on_connected(&self, session: &Session) {
        tracing::info!(
            "Session {} connected from {}",
            session.id(),
            session.peer_addr()
        );
    }

    async fn on_message(&self, session: &Session, message: Message) {
        tracing::debug!("Session {} received message: {}", session.id(), message);
    }

    async fn on_disconnected(&self, session: &Session) {
        tracing::info!(
            "Session {} disconnected (uptime: {:?})",
            session.id(),
            session.uptime()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id_generation() {
        // Test that session IDs are cryptographically random and unique
        let id1 = generate_session_id();
        let id2 = generate_session_id();

        // IDs should be different (probability of collision is 1 in 2^128)
        assert_ne!(id1, id2);

        // IDs should be non-zero (probability of zero is 1 in 2^128)
        assert_ne!(id1, 0);
        assert_ne!(id2, 0);
    }
}
