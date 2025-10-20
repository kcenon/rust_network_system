//! Graceful shutdown coordination

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

/// Graceful shutdown coordinator
pub struct GracefulShutdown {
    /// Shutdown signal
    shutdown_requested: Arc<AtomicBool>,
    /// Active request counter
    active_requests: Arc<AtomicUsize>,
    /// Notify when all requests complete
    all_completed: Arc<Notify>,
    /// Shutdown timeout
    timeout: Duration,
}

impl GracefulShutdown {
    /// Create a new graceful shutdown coordinator
    pub fn new(timeout: Duration) -> Self {
        Self {
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            active_requests: Arc::new(AtomicUsize::new(0)),
            all_completed: Arc::new(Notify::new()),
            timeout,
        }
    }

    /// Create with default timeout (30 seconds)
    pub fn with_default_timeout() -> Self {
        Self::new(Duration::from_secs(30))
    }

    /// Check if shutdown has been requested
    pub fn is_shutdown_requested(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    /// Get number of active requests
    pub fn active_request_count(&self) -> usize {
        self.active_requests.load(Ordering::Acquire)
    }

    /// Register a new active request
    pub fn enter(&self) -> Option<ShutdownGuard> {
        if self.is_shutdown_requested() {
            return None;
        }

        self.active_requests.fetch_add(1, Ordering::AcqRel);

        Some(ShutdownGuard {
            active_requests: Arc::clone(&self.active_requests),
            all_completed: Arc::clone(&self.all_completed),
        })
    }

    /// Request shutdown
    pub fn shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }

    /// Wait for all active requests to complete
    pub async fn wait_for_completion(&self) -> bool {
        let timeout_duration = self.timeout;

        // Wait with timeout
        tokio::select! {
            _ = self.wait_until_idle() => true,
            _ = tokio::time::sleep(timeout_duration) => false,
        }
    }

    /// Wait until no active requests remain
    async fn wait_until_idle(&self) {
        loop {
            if self.active_requests.load(Ordering::Acquire) == 0 {
                break;
            }

            self.all_completed.notified().await;
        }
    }

    /// Shutdown and wait for completion
    pub async fn shutdown_and_wait(&self) -> bool {
        self.shutdown();
        self.wait_for_completion().await
    }
}

impl Default for GracefulShutdown {
    fn default() -> Self {
        Self::with_default_timeout()
    }
}

/// RAII guard for tracking active requests
pub struct ShutdownGuard {
    active_requests: Arc<AtomicUsize>,
    all_completed: Arc<Notify>,
}

/// Automatic cleanup when ShutdownGuard is dropped
///
/// # Behavior
///
/// When a `ShutdownGuard` is dropped (goes out of scope), it:
/// 1. **Decrements** the active request counter atomically using `AcqRel` ordering
/// 2. **Notifies** all waiting tasks if this was the last active request
///
/// # Memory Ordering
///
/// Uses `Ordering::AcqRel` to ensure:
/// - **Acquire**: Synchronizes with all previous Release operations on the counter
/// - **Release**: Makes all memory modifications visible to subsequent Acquire operations
///
/// This guarantees that when the last request completes, all its side effects are visible
/// to tasks waiting on `wait_for_completion()`.
///
/// # Thread Safety
///
/// This Drop implementation is:
/// - **Lock-free**: Uses only atomic operations, no blocking
/// - **Panic-safe**: Always executes, even during stack unwinding
/// - **Async-safe**: Safe to drop from async contexts (no `.await`)
///
/// # Example
///
/// ```no_run
/// use rust_network_system::stability::GracefulShutdown;
/// use std::time::Duration;
///
/// # async fn example() {
/// let shutdown = GracefulShutdown::new(Duration::from_secs(30));
///
/// {
///     let guard = shutdown.enter().unwrap();
///     // Process request...
/// } // Guard dropped here - automatically decrements counter
/// # }
/// ```
impl Drop for ShutdownGuard {
    fn drop(&mut self) {
        // Decrement active request counter with AcqRel ordering
        // for proper synchronization with shutdown waiter
        let prev = self.active_requests.fetch_sub(1, Ordering::AcqRel);

        // If this was the last active request, notify waiters
        // This ensures wait_for_completion() can proceed
        if prev == 1 {
            self.all_completed.notify_waiters();
        }
    }
}

/// Shutdown signal for graceful termination
#[derive(Clone)]
pub struct ShutdownSignal {
    shutdown_requested: Arc<AtomicBool>,
}

impl ShutdownSignal {
    /// Create a new shutdown signal
    pub fn new() -> Self {
        Self {
            shutdown_requested: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Trigger shutdown
    pub fn trigger(&self) {
        self.shutdown_requested.store(true, Ordering::Release);
    }

    /// Check if shutdown was triggered
    pub fn is_triggered(&self) -> bool {
        self.shutdown_requested.load(Ordering::Acquire)
    }

    /// Wait for shutdown signal
    pub async fn wait(&self) {
        while !self.is_triggered() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Default for ShutdownSignal {
    fn default() -> Self {
        Self::new()
    }
}

/// Multi-component shutdown coordinator
pub struct ShutdownCoordinator {
    signals: Vec<ShutdownSignal>,
    graceful: GracefulShutdown,
}

impl ShutdownCoordinator {
    /// Create a new shutdown coordinator
    pub fn new(timeout: Duration) -> Self {
        Self {
            signals: Vec::new(),
            graceful: GracefulShutdown::new(timeout),
        }
    }

    /// Create a shutdown signal for a component
    pub fn create_signal(&mut self) -> ShutdownSignal {
        let signal = ShutdownSignal::new();
        self.signals.push(signal.clone());
        signal
    }

    /// Get the graceful shutdown handler
    pub fn graceful(&self) -> &GracefulShutdown {
        &self.graceful
    }

    /// Shutdown all components and wait
    pub async fn shutdown_all(&self) -> bool {
        // Trigger all component signals
        for signal in &self.signals {
            signal.trigger();
        }

        // Wait for graceful shutdown
        self.graceful.shutdown_and_wait().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_shutdown_signal() {
        let signal = ShutdownSignal::new();

        assert!(!signal.is_triggered());

        signal.trigger();

        assert!(signal.is_triggered());
    }

    #[tokio::test]
    async fn test_shutdown_wait() {
        let signal = ShutdownSignal::new();
        let signal_clone = signal.clone();

        // Trigger after delay
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            signal_clone.trigger();
        });

        // Wait for signal
        let start = tokio::time::Instant::now();
        signal.wait().await;
        let elapsed = start.elapsed();

        assert!(elapsed >= Duration::from_millis(50));
    }

    #[tokio::test]
    async fn test_graceful_shutdown_tracking() {
        let shutdown = GracefulShutdown::new(Duration::from_secs(1));

        assert_eq!(shutdown.active_request_count(), 0);

        let guard1 = shutdown.enter().unwrap();
        assert_eq!(shutdown.active_request_count(), 1);

        let guard2 = shutdown.enter().unwrap();
        assert_eq!(shutdown.active_request_count(), 2);

        drop(guard1);
        assert_eq!(shutdown.active_request_count(), 1);

        drop(guard2);
        assert_eq!(shutdown.active_request_count(), 0);
    }

    #[tokio::test]
    async fn test_reject_after_shutdown() {
        let shutdown = GracefulShutdown::new(Duration::from_secs(1));

        shutdown.shutdown();

        // Should reject new requests after shutdown
        assert!(shutdown.enter().is_none());
    }

    #[tokio::test]
    async fn test_wait_for_completion() {
        let shutdown = GracefulShutdown::new(Duration::from_secs(2));

        let guard = shutdown.enter().unwrap();

        // Spawn task to drop guard after delay
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            drop(guard);
        });

        // Wait for completion
        let start = tokio::time::Instant::now();
        let completed = shutdown.wait_for_completion().await;
        let elapsed = start.elapsed();

        assert!(completed);
        assert!(elapsed >= Duration::from_millis(100));
    }

    #[tokio::test]
    async fn test_timeout() {
        let shutdown = GracefulShutdown::new(Duration::from_millis(100));

        let _guard = shutdown.enter().unwrap();

        // Don't drop guard, should timeout
        let completed = shutdown.wait_for_completion().await;

        assert!(!completed); // Should timeout
    }

    #[tokio::test]
    async fn test_shutdown_and_wait() {
        let shutdown = GracefulShutdown::new(Duration::from_secs(1));

        let guard = shutdown.enter().unwrap();

        // Spawn task to drop guard
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            drop(guard);
        });

        let completed = shutdown.shutdown_and_wait().await;
        assert!(completed);
        assert!(shutdown.is_shutdown_requested());
    }

    #[tokio::test]
    async fn test_coordinator() {
        let mut coordinator = ShutdownCoordinator::new(Duration::from_secs(1));

        let signal1 = coordinator.create_signal();
        let signal2 = coordinator.create_signal();

        assert!(!signal1.is_triggered());
        assert!(!signal2.is_triggered());

        coordinator.shutdown_all().await;

        assert!(signal1.is_triggered());
        assert!(signal2.is_triggered());
    }
}
