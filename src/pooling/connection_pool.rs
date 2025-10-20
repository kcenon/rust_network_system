//! Generic connection pooling for efficient resource management

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;

/// Connection pool configuration
#[derive(Debug, Clone)]
pub struct PoolConfig {
    /// Minimum number of connections to maintain
    pub min_size: usize,
    /// Maximum number of connections allowed
    pub max_size: usize,
    /// Maximum time to wait for a connection
    pub acquire_timeout: Duration,
    /// Connection idle timeout (after which it's closed)
    pub idle_timeout: Duration,
    /// Maximum lifetime of a connection
    pub max_lifetime: Option<Duration>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            min_size: 2,
            max_size: 10,
            acquire_timeout: Duration::from_secs(5),
            idle_timeout: Duration::from_secs(600),
            max_lifetime: Some(Duration::from_secs(3600)),
        }
    }
}

impl PoolConfig {
    /// Create a new pool configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Set minimum pool size
    pub fn with_min_size(mut self, size: usize) -> Self {
        self.min_size = size;
        self
    }

    /// Set maximum pool size
    pub fn with_max_size(mut self, size: usize) -> Self {
        self.max_size = size;
        self
    }

    /// Set acquire timeout
    pub fn with_acquire_timeout(mut self, timeout: Duration) -> Self {
        self.acquire_timeout = timeout;
        self
    }

    /// Set idle timeout
    pub fn with_idle_timeout(mut self, timeout: Duration) -> Self {
        self.idle_timeout = timeout;
        self
    }

    /// Set maximum connection lifetime
    pub fn with_max_lifetime(mut self, lifetime: Option<Duration>) -> Self {
        self.max_lifetime = lifetime;
        self
    }
}

/// Trait for types that can be pooled
#[async_trait::async_trait]
pub trait Poolable: Send + Sized {
    /// Error type for connection operations
    type Error: std::error::Error + Send + Sync + 'static;

    /// Create a new connection
    async fn create() -> Result<Self, Self::Error>;

    /// Check if the connection is still valid
    async fn is_valid(&self) -> bool;

    /// Close the connection
    async fn close(self) -> Result<(), Self::Error>;
}

/// Pooled connection wrapper
struct PooledConnection<T> {
    connection: T,
    created_at: Instant,
    last_used: Instant,
}

impl<T> PooledConnection<T> {
    fn new(connection: T) -> Self {
        let now = Instant::now();
        Self {
            connection,
            created_at: now,
            last_used: now,
        }
    }

    fn is_expired(&self, config: &PoolConfig) -> bool {
        // Check idle timeout
        if self.last_used.elapsed() > config.idle_timeout {
            return true;
        }

        // Check max lifetime
        if let Some(max_lifetime) = config.max_lifetime {
            if self.created_at.elapsed() > max_lifetime {
                return true;
            }
        }

        false
    }

    fn touch(&mut self) {
        self.last_used = Instant::now();
    }
}

/// Internal pool state
struct PoolState<T> {
    available: VecDeque<PooledConnection<T>>,
    total_connections: usize,
}

/// Generic connection pool
pub struct ConnectionPool<T: Poolable> {
    config: PoolConfig,
    state: Arc<parking_lot::Mutex<PoolState<T>>>,
    semaphore: Arc<Semaphore>,
}

impl<T: Poolable> ConnectionPool<T> {
    /// Create a new connection pool
    pub fn new(config: PoolConfig) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(config.max_size)),
            config,
            state: Arc::new(parking_lot::Mutex::new(PoolState {
                available: VecDeque::new(),
                total_connections: 0,
            })),
        }
    }

    /// Create a pool with default configuration
    pub fn with_default_config() -> Self {
        Self::new(PoolConfig::default())
    }

    /// Get current pool statistics
    pub fn stats(&self) -> PoolStats {
        let state = self.state.lock();
        PoolStats {
            available: state.available.len(),
            total: state.total_connections,
            max_size: self.config.max_size,
        }
    }

    /// Acquire a connection from the pool
    pub async fn acquire(&self) -> Result<PooledGuard<T>, PoolError<T::Error>> {
        // Try to acquire permit with timeout
        // Use acquire_owned() to get OwnedSemaphorePermit (no lifetime parameter)
        let permit = tokio::time::timeout(
            self.config.acquire_timeout,
            self.semaphore.clone().acquire_owned(),
        )
        .await
        .map_err(|_| PoolError::Timeout)?
        .map_err(|_| PoolError::Closed)?;

        // Try to get an available connection
        let (connection, expired) = {
            let mut state = self.state.lock();

            // Remove expired connections and collect them for cleanup
            let expired = self.remove_expired(&mut state);

            // Try to reuse an existing connection
            let conn = if let Some(mut pooled) = state.available.pop_front() {
                pooled.touch();
                Some(pooled.connection)
            } else {
                None
            };

            (conn, expired)
        };

        // Close expired connections after releasing the lock
        for conn in expired {
            let _ = conn.close().await;
        }

        let connection = if let Some(conn) = connection {
            // Validate the connection
            if conn.is_valid().await {
                conn
            } else {
                // Connection is invalid, create a new one
                self.create_connection().await?
            }
        } else {
            // No available connection, create a new one
            self.create_connection().await?
        };

        Ok(PooledGuard {
            connection: Some(connection),
            pool: Arc::clone(&self.state),
            config: self.config.clone(),
            _permit: permit,
        })
    }

    /// Create a new connection and track it
    async fn create_connection(&self) -> Result<T, PoolError<T::Error>> {
        let connection = T::create().await.map_err(PoolError::Create)?;

        {
            let mut state = self.state.lock();
            state.total_connections += 1;
        }

        Ok(connection)
    }

    /// Remove expired connections from the pool and return them for cleanup
    fn remove_expired(&self, state: &mut PoolState<T>) -> Vec<T> {
        let mut to_remove = Vec::new();

        for (idx, conn) in state.available.iter().enumerate() {
            if conn.is_expired(&self.config) {
                to_remove.push(idx);
            }
        }

        let mut expired = Vec::new();

        // Remove in reverse order to maintain indices
        for idx in to_remove.into_iter().rev() {
            if let Some(pooled_conn) = state.available.remove(idx) {
                state.total_connections -= 1;
                expired.push(pooled_conn.connection);
            }
        }

        expired
    }

    /// Prune idle connections (manual cleanup)
    pub async fn prune_idle(&self) {
        // Calculate needed connections in a block to ensure lock is dropped
        let (needed, expired) = {
            let mut state = self.state.lock();
            let expired = self.remove_expired(&mut state);
            // Return needed before state is dropped at end of block
            let needed = self.config.min_size.saturating_sub(state.total_connections);
            (needed, expired)
        }; // MutexGuard is dropped here

        // Close expired connections after releasing the lock
        for conn in expired {
            let _ = conn.close().await;
        }

        for _ in 0..needed {
            if let Ok(conn) = T::create().await {
                let mut state = self.state.lock();
                state.available.push_back(PooledConnection::new(conn));
                state.total_connections += 1;
            }
        }
    }

    /// Start background maintenance task
    pub fn start_maintenance(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()>
    where
        T: Send + Sync + 'static,
    {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(interval);
            loop {
                interval.tick().await;
                self.prune_idle().await;
            }
        })
    }

    /// Explicitly close all pooled connections
    ///
    /// This should be called before dropping the pool to ensure proper cleanup
    pub async fn shutdown(&self) {
        let connections = {
            let mut state = self.state.lock();
            let mut conns = Vec::new();
            while let Some(pooled) = state.available.pop_front() {
                conns.push(pooled.connection);
                state.total_connections -= 1;
            }
            conns
        };

        // Close all connections after releasing the lock
        for conn in connections {
            let _ = conn.close().await;
        }
    }
}

impl<T: Poolable> Drop for ConnectionPool<T> {
    fn drop(&mut self) {
        let state = self.state.lock();
        if state.total_connections > 0 {
            tracing::warn!(
                "ConnectionPool dropped with {} connections still active. Call shutdown() before dropping to properly close connections.",
                state.total_connections
            );
        }
        // Note: Cannot call async close() from Drop, so connections will be dropped without cleanup
        // Users should call shutdown() explicitly before dropping the pool
    }
}

/// RAII guard for pooled connections
pub struct PooledGuard<T: Poolable> {
    connection: Option<T>,
    pool: Arc<parking_lot::Mutex<PoolState<T>>>,
    #[allow(dead_code)] // Reserved for future connection validation/debugging
    config: PoolConfig,
    _permit: tokio::sync::OwnedSemaphorePermit,
}

impl<T: Poolable> std::convert::AsRef<T> for PooledGuard<T> {
    fn as_ref(&self) -> &T {
        // Invariant: connection is always Some between acquire() and Drop::drop()
        // This invariant is maintained by the PooledGuard API design:
        // - Set to Some in acquire()
        // - Only taken in Drop
        // We use expect() instead of unwrap_unchecked() to maintain safety in release builds
        self.connection
            .as_ref()
            .expect("PooledGuard invariant violated: connection is None before Drop")
    }
}

impl<T: Poolable> std::convert::AsMut<T> for PooledGuard<T> {
    fn as_mut(&mut self) -> &mut T {
        // Invariant: connection is always Some between acquire() and Drop::drop()
        // This invariant is maintained by the PooledGuard API design
        // We use expect() instead of unwrap_unchecked() to maintain safety in release builds
        self.connection
            .as_mut()
            .expect("PooledGuard invariant violated: connection is None before Drop")
    }
}

impl<T: Poolable> std::ops::Deref for PooledGuard<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        // Invariant: connection is always Some between acquire() and Drop::drop()
        // This invariant is maintained by the PooledGuard API design:
        // - Set to Some in acquire()
        // - Only taken in Drop
        // We use expect() instead of unwrap_unchecked() to maintain safety in release builds
        self.connection
            .as_ref()
            .expect("PooledGuard invariant violated: connection is None before Drop")
    }
}

impl<T: Poolable> std::ops::DerefMut for PooledGuard<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // Invariant: connection is always Some between acquire() and Drop::drop()
        // This invariant is maintained by the PooledGuard API design
        // We use expect() instead of unwrap_unchecked() to maintain safety in release builds
        self.connection
            .as_mut()
            .expect("PooledGuard invariant violated: connection is None before Drop")
    }
}

impl<T: Poolable> Drop for PooledGuard<T> {
    fn drop(&mut self) {
        if let Some(connection) = self.connection.take() {
            let mut state = self.pool.lock();
            state.available.push_back(PooledConnection::new(connection));
        }
    }
}

/// Pool statistics
#[derive(Debug, Clone)]
pub struct PoolStats {
    /// Number of available connections
    pub available: usize,
    /// Total number of connections
    pub total: usize,
    /// Maximum pool size
    pub max_size: usize,
}

/// Pool errors
#[derive(Debug)]
pub enum PoolError<E> {
    /// Failed to create connection
    Create(E),
    /// Timeout waiting for connection
    Timeout,
    /// Pool is closed
    Closed,
}

impl<E: std::fmt::Display> std::fmt::Display for PoolError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PoolError::Create(e) => write!(f, "Failed to create connection: {}", e),
            PoolError::Timeout => write!(f, "Timeout waiting for connection"),
            PoolError::Closed => write!(f, "Pool is closed"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for PoolError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            PoolError::Create(e) => Some(e),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mock connection for testing
    struct MockConnection {
        id: u32,
        fail_validation: bool,
    }

    #[derive(Debug)]
    struct MockError;

    impl std::fmt::Display for MockError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Mock error")
        }
    }

    impl std::error::Error for MockError {}

    static NEXT_ID: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

    #[async_trait::async_trait]
    impl Poolable for MockConnection {
        type Error = MockError;

        async fn create() -> Result<Self, Self::Error> {
            let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(Self {
                id,
                fail_validation: false,
            })
        }

        async fn is_valid(&self) -> bool {
            !self.fail_validation
        }

        async fn close(self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_pool_creation() {
        let config = PoolConfig::new().with_max_size(5);
        let pool = ConnectionPool::<MockConnection>::new(config);

        let stats = pool.stats();
        assert_eq!(stats.available, 0);
        assert_eq!(stats.total, 0);
        assert_eq!(stats.max_size, 5);
    }

    #[tokio::test]
    async fn test_acquire_connection() {
        let pool = ConnectionPool::<MockConnection>::with_default_config();

        let conn = pool.acquire().await.unwrap();
        // Don't check specific ID due to test isolation issues with static NEXT_ID
        // Just verify connection was acquired
        assert!(conn.id >= 1);

        let stats = pool.stats();
        assert_eq!(stats.total, 1);
    }

    #[tokio::test]
    async fn test_connection_reuse() {
        let pool = ConnectionPool::<MockConnection>::with_default_config();

        let conn1_id = {
            let conn = pool.acquire().await.unwrap();
            conn.id
        };

        // Connection should be returned to pool
        let stats = pool.stats();
        assert_eq!(stats.available, 1);

        // Acquire again should reuse
        let conn2 = pool.acquire().await.unwrap();
        assert_eq!(conn2.id, conn1_id);
    }

    #[tokio::test]
    async fn test_max_connections() {
        let config = PoolConfig::new().with_max_size(2);
        let pool = Arc::new(ConnectionPool::<MockConnection>::new(config));

        let _conn1 = pool.acquire().await.unwrap();
        let _conn2 = pool.acquire().await.unwrap();

        let stats = pool.stats();
        assert_eq!(stats.total, 2);

        // Third acquire should timeout
        let config = PoolConfig::new()
            .with_max_size(2)
            .with_acquire_timeout(Duration::from_millis(100));
        let pool = Arc::new(ConnectionPool::<MockConnection>::new(config));

        let _c1 = pool.acquire().await.unwrap();
        let _c2 = pool.acquire().await.unwrap();

        let result = pool.acquire().await;
        assert!(matches!(result, Err(PoolError::Timeout)));
    }

    #[tokio::test]
    async fn test_concurrent_access() {
        let pool = Arc::new(ConnectionPool::<MockConnection>::with_default_config());

        let mut handles = vec![];
        for _ in 0..10 {
            let pool_clone = Arc::clone(&pool);
            let handle = tokio::spawn(async move {
                let _conn = pool_clone.acquire().await.unwrap();
                tokio::time::sleep(Duration::from_millis(10)).await;
            });
            handles.push(handle);
        }

        for handle in handles {
            handle.await.unwrap();
        }

        let stats = pool.stats();
        assert!(stats.total <= 10);
    }
}
