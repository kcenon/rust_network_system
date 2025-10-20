//! Advanced integration tests for Circuit Breaker and Connection Pool
//!
//! This test suite covers:
//! - Circuit breaker integration with network operations
//! - Connection pool stress testing under high concurrency
//!
//! Note: TLS integration tests require API enhancements to ServerConfig and TcpClient
//! and are not included in this version.

use async_trait::async_trait;
use rust_network_system::{Message, ServerConfig, Session, SessionHandler, TcpClient, TcpServer};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc,
};
use std::time::Duration;

// ============================================================================
// Circuit Breaker Integration Tests
// ============================================================================

mod circuit_breaker_tests {
    use super::*;
    use rust_network_system::stability::{CircuitBreaker, CircuitBreakerConfig, CircuitState};

    /// Handler that can simulate failures
    struct FlakyHandler {
        should_fail: Arc<parking_lot::RwLock<bool>>,
        request_count: Arc<AtomicU32>,
    }

    impl FlakyHandler {
        fn new() -> Self {
            Self {
                should_fail: Arc::new(parking_lot::RwLock::new(false)),
                request_count: Arc::new(AtomicU32::new(0)),
            }
        }

        fn set_failure_mode(&self, fail: bool) {
            *self.should_fail.write() = fail;
        }
    }

    #[async_trait]
    impl SessionHandler for FlakyHandler {
        async fn on_connected(&self, _session: &Session) {}

        async fn on_message(&self, session: &Session, message: Message) {
            self.request_count.fetch_add(1, Ordering::SeqCst);

            if *self.should_fail.read() {
                // Simulate failure by not responding
                return;
            }

            let _ = session.send(&message).await;
        }

        async fn on_disconnected(&self, _session: &Session) {}
    }

    #[tokio::test]
    async fn test_circuit_breaker_opens_on_failures() {
        let config = CircuitBreakerConfig::new()
            .with_failure_threshold(3)
            .with_timeout(Duration::from_secs(1));
        let circuit_breaker = CircuitBreaker::with_config(config);

        // Simulate 3 failures
        for _ in 0..3 {
            assert!(circuit_breaker.is_request_allowed());
            circuit_breaker.record_failure();
        }

        // Circuit should now be open
        assert_eq!(circuit_breaker.state(), CircuitState::Open);
        assert!(!circuit_breaker.is_request_allowed());
    }

    #[tokio::test]
    async fn test_circuit_breaker_with_network_operations() {
        let handler = Arc::new(FlakyHandler::new());
        let handler_clone = Arc::clone(&handler);

        // Start server with flaky handler
        let config = ServerConfig::new("127.0.0.1:10010");
        let mut server = TcpServer::with_config(config);
        server
            .start(handler_clone)
            .await
            .expect("Failed to start server");

        tokio::time::sleep(Duration::from_millis(100)).await;

        // Circuit breaker with low threshold for testing
        let cb_config = CircuitBreakerConfig::new()
            .with_failure_threshold(3)
            .with_timeout(Duration::from_millis(500));
        let circuit_breaker = Arc::new(CircuitBreaker::with_config(cb_config));

        // Connect client
        let mut client = TcpClient::new("127.0.0.1:10010");
        client.connect().await.expect("Failed to connect");

        // Test successful requests
        for i in 0..5 {
            if !circuit_breaker.is_request_allowed() {
                break;
            }

            let msg = Message::new(format!("Request {}", i));
            let result = tokio::time::timeout(Duration::from_millis(100), async {
                client.send(&msg).await?;
                client.receive().await
            })
            .await;

            if result.is_ok() && result.unwrap().is_ok() {
                circuit_breaker.record_success();
            } else {
                circuit_breaker.record_failure();
            }
        }

        assert_eq!(circuit_breaker.state(), CircuitState::Closed);

        // Enable failure mode
        handler.set_failure_mode(true);

        // Trigger failures
        for i in 0..5 {
            if !circuit_breaker.is_request_allowed() {
                break;
            }

            let msg = Message::new(format!("Failing request {}", i));
            let result = tokio::time::timeout(Duration::from_millis(100), async {
                client.send(&msg).await?;
                client.receive().await
            })
            .await;

            if result.is_ok() && result.unwrap().is_ok() {
                circuit_breaker.record_success();
            } else {
                circuit_breaker.record_failure();
            }
        }

        // Circuit should be open after failures
        assert_eq!(circuit_breaker.state(), CircuitState::Open);
        assert!(!circuit_breaker.is_request_allowed());

        // Cleanup
        client.disconnect().await.ok();
        server.stop().await.expect("Failed to stop server");
    }

    #[tokio::test]
    async fn test_circuit_breaker_recovery() {
        let config = CircuitBreakerConfig::new()
            .with_failure_threshold(2)
            .with_success_threshold(2)
            .with_timeout(Duration::from_millis(200));
        let circuit_breaker = CircuitBreaker::with_config(config);

        // Open the circuit
        circuit_breaker.record_failure();
        circuit_breaker.record_failure();
        assert_eq!(circuit_breaker.state(), CircuitState::Open);

        // Wait for timeout
        tokio::time::sleep(Duration::from_millis(250)).await;

        // Should transition to half-open
        assert!(circuit_breaker.is_request_allowed());
        assert_eq!(circuit_breaker.state(), CircuitState::HalfOpen);

        // Record successes to close the circuit
        circuit_breaker.record_success();
        circuit_breaker.record_success();

        assert_eq!(circuit_breaker.state(), CircuitState::Closed);
        assert!(circuit_breaker.is_request_allowed());
    }
}

// ============================================================================
// Connection Pool Stress Tests
// ============================================================================

mod connection_pool_stress_tests {
    use super::*;
    use rust_network_system::pooling::{ConnectionPool, PoolConfig, PoolError, Poolable};

    /// Mock poolable connection for testing
    #[derive(Debug)]
    struct MockNetworkConnection {
        id: u32,
        created_at: std::time::Instant,
    }

    #[derive(Debug)]
    struct MockError(String);

    impl std::fmt::Display for MockError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(f, "Mock error: {}", self.0)
        }
    }

    impl std::error::Error for MockError {}

    static CONNECTION_ID: AtomicU32 = AtomicU32::new(1);

    #[async_trait]
    impl Poolable for MockNetworkConnection {
        type Error = MockError;

        async fn create() -> Result<Self, Self::Error> {
            // Simulate connection creation delay
            tokio::time::sleep(Duration::from_millis(10)).await;

            let id = CONNECTION_ID.fetch_add(1, Ordering::SeqCst);
            Ok(Self {
                id,
                created_at: std::time::Instant::now(),
            })
        }

        async fn is_valid(&self) -> bool {
            // Simulate validation check
            self.created_at.elapsed() < Duration::from_secs(300)
        }

        async fn close(self) -> Result<(), Self::Error> {
            // Simulate cleanup
            tokio::time::sleep(Duration::from_millis(5)).await;
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_pool_high_concurrency() {
        let config = PoolConfig::new()
            .with_min_size(5)
            .with_max_size(50)
            .with_acquire_timeout(Duration::from_secs(5));

        let pool = Arc::new(ConnectionPool::<MockNetworkConnection>::new(config));

        // Spawn many concurrent tasks
        let mut handles = vec![];
        for i in 0..200 {
            let pool_clone = Arc::clone(&pool);
            let handle = tokio::spawn(async move {
                match pool_clone.acquire().await {
                    Ok(conn) => {
                        // Simulate work with the connection
                        tokio::time::sleep(Duration::from_millis(5)).await;
                        (i, conn.id, true)
                    }
                    Err(_) => (i, 0, false),
                }
            });
            handles.push(handle);
        }

        // Collect results
        let mut success_count = 0;
        let mut unique_connections = std::collections::HashSet::new();

        for handle in handles {
            let (_, conn_id, success) = handle.await.expect("Task failed");
            if success {
                success_count += 1;
                unique_connections.insert(conn_id);
            }
        }

        // Verify all tasks succeeded
        assert_eq!(success_count, 200, "All tasks should acquire connections");

        // Verify connection reuse (should have far fewer connections than requests)
        assert!(
            unique_connections.len() <= 50,
            "Should reuse connections efficiently, got {} unique connections",
            unique_connections.len()
        );

        pool.shutdown().await;
    }

    #[tokio::test]
    async fn test_pool_timeout_under_load() {
        let config = PoolConfig::new()
            .with_max_size(5)
            .with_acquire_timeout(Duration::from_millis(100));

        let pool = Arc::new(ConnectionPool::<MockNetworkConnection>::new(config));

        // Acquire all connections and hold them
        let mut guards = vec![];
        for _ in 0..5 {
            let guard = pool.acquire().await.expect("Should acquire connection");
            guards.push(guard);
        }

        // Try to acquire another connection (should timeout)
        let result = pool.acquire().await;
        assert!(
            matches!(result, Err(PoolError::Timeout)),
            "Should timeout when pool is exhausted"
        );

        // Release one connection
        guards.pop();
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Now should be able to acquire
        let result = pool.acquire().await;
        assert!(
            result.is_ok(),
            "Should acquire after connection is released"
        );

        pool.shutdown().await;
    }

    #[tokio::test]
    async fn test_pool_stress_with_rapid_acquire_release() {
        let config = PoolConfig::new()
            .with_min_size(2)
            .with_max_size(20)
            .with_acquire_timeout(Duration::from_secs(10));

        let pool = Arc::new(ConnectionPool::<MockNetworkConnection>::new(config));

        // Rapidly acquire and release connections
        let mut handles = vec![];
        for iteration in 0..100 {
            let pool_clone = Arc::clone(&pool);
            let handle = tokio::spawn(async move {
                for _ in 0..10 {
                    let _conn = pool_clone.acquire().await.expect("Should acquire");
                    // Immediate drop (rapid release)
                }
                iteration
            });
            handles.push(handle);
        }

        // Wait for all to complete
        for handle in handles {
            handle.await.expect("Task should complete");
        }

        let stats = pool.stats();
        assert!(
            stats.available > 0,
            "Should have available connections after stress test"
        );
        assert!(
            stats.total <= 20,
            "Should not exceed max pool size, got {}",
            stats.total
        );

        pool.shutdown().await;
    }

    #[tokio::test]
    async fn test_pool_connection_lifecycle() {
        let config = PoolConfig::new()
            .with_min_size(1)
            .with_max_size(10)
            .with_idle_timeout(Duration::from_millis(100))
            .with_max_lifetime(Some(Duration::from_millis(200)));

        let pool = Arc::new(ConnectionPool::<MockNetworkConnection>::new(config));

        // Acquire and release connection
        let conn_id = {
            let conn = pool.acquire().await.expect("Should acquire");
            conn.id
        };

        let stats = pool.stats();
        assert_eq!(stats.available, 1, "Connection should be returned to pool");

        // Wait for connection to expire
        tokio::time::sleep(Duration::from_millis(250)).await;

        // Prune idle connections
        pool.prune_idle().await;

        // Acquire again - should get a new connection due to expiration
        let new_conn = pool.acquire().await.expect("Should acquire new connection");
        assert_ne!(
            conn_id, new_conn.id,
            "Should create new connection after expiration"
        );

        pool.shutdown().await;
    }

    #[tokio::test]
    async fn test_pool_maintenance_task() {
        let config = PoolConfig::new().with_min_size(3).with_max_size(10);

        let min_size = config.min_size;
        let pool = Arc::new(ConnectionPool::<MockNetworkConnection>::new(config));

        // Start maintenance task
        let maintenance_handle = pool.clone().start_maintenance(Duration::from_millis(50));

        // Give maintenance time to run
        tokio::time::sleep(Duration::from_millis(200)).await;

        let stats = pool.stats();
        assert!(
            stats.available >= min_size,
            "Maintenance should ensure minimum connections, got {} available",
            stats.available
        );

        // Cleanup
        maintenance_handle.abort();
        pool.shutdown().await;
    }

    #[tokio::test]
    async fn test_pool_mixed_workload() {
        let config = PoolConfig::new()
            .with_max_size(30)
            .with_acquire_timeout(Duration::from_secs(5));

        let pool = Arc::new(ConnectionPool::<MockNetworkConnection>::new(config));

        // Mix of short and long-lived connections
        let mut handles = vec![];

        // Long-lived connections (10 tasks)
        for i in 0..10 {
            let pool_clone = Arc::clone(&pool);
            let handle = tokio::spawn(async move {
                let _conn = pool_clone.acquire().await.expect("Should acquire");
                tokio::time::sleep(Duration::from_millis(100)).await;
                i
            });
            handles.push(handle);
        }

        // Short-lived rapid connections (50 tasks)
        for i in 10..60 {
            let pool_clone = Arc::clone(&pool);
            let handle = tokio::spawn(async move {
                let _conn = pool_clone.acquire().await.expect("Should acquire");
                tokio::time::sleep(Duration::from_millis(5)).await;
                i
            });
            handles.push(handle);
        }

        // Wait for all tasks
        for handle in handles {
            handle.await.expect("Task should complete");
        }

        let stats = pool.stats();
        assert!(
            stats.total <= 30,
            "Should not exceed max size under mixed workload"
        );

        pool.shutdown().await;
    }
}
