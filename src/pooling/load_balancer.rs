//! Load balancing with multiple strategies and health checks

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Backend server endpoint
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Backend {
    /// Server address
    pub address: String,
    /// Server weight (for weighted strategies)
    pub weight: u32,
}

impl Backend {
    /// Create a new backend
    pub fn new(address: impl Into<String>) -> Self {
        Self {
            address: address.into(),
            weight: 1,
        }
    }

    /// Create a backend with weight
    pub fn with_weight(address: impl Into<String>, weight: u32) -> Self {
        Self {
            address: address.into(),
            weight,
        }
    }
}

/// Health check status
#[derive(Debug, Clone)]
pub struct HealthStatus {
    /// Is the backend healthy
    pub healthy: bool,
    /// Last check time
    pub last_check: Instant,
    /// Consecutive failures
    pub consecutive_failures: u32,
    /// Total requests
    pub total_requests: u64,
    /// Failed requests
    pub failed_requests: u64,
}

impl HealthStatus {
    #[allow(dead_code)] // Reserved for future use or testing
    fn new() -> Self {
        Self {
            healthy: true,
            last_check: Instant::now(),
            consecutive_failures: 0,
            total_requests: 0,
            failed_requests: 0,
        }
    }

    /// Calculate error rate
    pub fn error_rate(&self) -> f64 {
        if self.total_requests == 0 {
            0.0
        } else {
            self.failed_requests as f64 / self.total_requests as f64
        }
    }
}

/// Backend state with health tracking
struct BackendState {
    backend: Backend,
    healthy: AtomicBool,
    active_connections: AtomicUsize,
    total_requests: AtomicU64,
    failed_requests: AtomicU64,
    consecutive_failures: AtomicU32,
    last_check: parking_lot::Mutex<Instant>,
}

impl BackendState {
    fn new(backend: Backend) -> Self {
        Self {
            backend,
            healthy: AtomicBool::new(true),
            active_connections: AtomicUsize::new(0),
            total_requests: AtomicU64::new(0),
            failed_requests: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
            last_check: parking_lot::Mutex::new(Instant::now()),
        }
    }

    fn is_healthy(&self) -> bool {
        self.healthy.load(Ordering::Acquire)
    }

    fn mark_healthy(&self) {
        self.healthy.store(true, Ordering::Release);
        self.consecutive_failures.store(0, Ordering::Release);
        *self.last_check.lock() = Instant::now();
    }

    fn mark_unhealthy(&self) {
        self.healthy.store(false, Ordering::Release);
        *self.last_check.lock() = Instant::now();
    }

    fn record_request(&self, success: bool) {
        self.total_requests.fetch_add(1, Ordering::Relaxed);

        if success {
            self.consecutive_failures.store(0, Ordering::Release);
        } else {
            self.failed_requests.fetch_add(1, Ordering::Relaxed);
            self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn get_status(&self) -> HealthStatus {
        HealthStatus {
            healthy: self.is_healthy(),
            last_check: *self.last_check.lock(),
            consecutive_failures: self.consecutive_failures.load(Ordering::Acquire),
            total_requests: self.total_requests.load(Ordering::Acquire),
            failed_requests: self.failed_requests.load(Ordering::Acquire),
        }
    }
}

/// Load balancing strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// Round-robin selection
    RoundRobin,
    /// Least connections
    LeastConnections,
    /// Weighted round-robin
    WeightedRoundRobin,
    /// Random selection
    Random,
}

/// Health check configuration
#[derive(Debug, Clone)]
pub struct HealthCheckConfig {
    /// Interval between health checks
    pub interval: Duration,
    /// Timeout for health checks
    pub timeout: Duration,
    /// Number of failures before marking unhealthy
    pub unhealthy_threshold: u32,
    /// Number of successes before marking healthy
    pub healthy_threshold: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            timeout: Duration::from_secs(2),
            unhealthy_threshold: 3,
            healthy_threshold: 2,
        }
    }
}

/// Load balancer configuration
#[derive(Debug, Clone)]
pub struct LoadBalancerConfig {
    /// Load balancing strategy
    pub strategy: Strategy,
    /// Health check configuration
    pub health_check: Option<HealthCheckConfig>,
}

impl Default for LoadBalancerConfig {
    fn default() -> Self {
        Self {
            strategy: Strategy::RoundRobin,
            health_check: Some(HealthCheckConfig::default()),
        }
    }
}

impl LoadBalancerConfig {
    /// Create a new configuration
    pub fn new(strategy: Strategy) -> Self {
        Self {
            strategy,
            health_check: Some(HealthCheckConfig::default()),
        }
    }

    /// Disable health checks
    pub fn without_health_checks(mut self) -> Self {
        self.health_check = None;
        self
    }
}

/// Load balancer
pub struct LoadBalancer {
    config: LoadBalancerConfig,
    backends: Arc<parking_lot::RwLock<Vec<Arc<BackendState>>>>,
    round_robin_index: AtomicUsize,
}

impl LoadBalancer {
    /// Create a new load balancer
    pub fn new(config: LoadBalancerConfig, backends: Vec<Backend>) -> Self {
        let backend_states: Vec<Arc<BackendState>> = backends
            .into_iter()
            .map(|b| Arc::new(BackendState::new(b)))
            .collect();

        Self {
            config,
            backends: Arc::new(parking_lot::RwLock::new(backend_states)),
            round_robin_index: AtomicUsize::new(0),
        }
    }

    /// Create with round-robin strategy
    pub fn round_robin(backends: Vec<Backend>) -> Self {
        Self::new(LoadBalancerConfig::new(Strategy::RoundRobin), backends)
    }

    /// Create with least connections strategy
    pub fn least_connections(backends: Vec<Backend>) -> Self {
        Self::new(
            LoadBalancerConfig::new(Strategy::LeastConnections),
            backends,
        )
    }

    /// Select next backend
    pub fn select(&self) -> Option<BackendGuard> {
        let backends = self.backends.read();

        if backends.is_empty() {
            return None;
        }

        let healthy_backends: Vec<_> = backends.iter().filter(|b| b.is_healthy()).collect();

        if healthy_backends.is_empty() {
            return None;
        }

        let selected = match self.config.strategy {
            Strategy::RoundRobin => self.select_round_robin(&healthy_backends),
            Strategy::LeastConnections => self.select_least_connections(&healthy_backends),
            Strategy::WeightedRoundRobin => self.select_weighted_round_robin(&healthy_backends),
            Strategy::Random => self.select_random(&healthy_backends),
        }?;

        selected.active_connections.fetch_add(1, Ordering::Relaxed);

        Some(BackendGuard {
            backend: selected.backend.clone(),
            state: Arc::clone(selected),
        })
    }

    /// Round-robin selection
    fn select_round_robin<'a>(
        &self,
        backends: &[&'a Arc<BackendState>],
    ) -> Option<&'a Arc<BackendState>> {
        let index = self.round_robin_index.fetch_add(1, Ordering::Relaxed);
        backends.get(index % backends.len()).copied()
    }

    /// Least connections selection
    fn select_least_connections<'a>(
        &self,
        backends: &[&'a Arc<BackendState>],
    ) -> Option<&'a Arc<BackendState>> {
        backends
            .iter()
            .min_by_key(|b| b.active_connections.load(Ordering::Acquire))
            .copied()
    }

    /// Weighted round-robin selection
    fn select_weighted_round_robin<'a>(
        &self,
        backends: &[&'a Arc<BackendState>],
    ) -> Option<&'a Arc<BackendState>> {
        // Simple weighted selection based on weight
        let total_weight: u32 = backends.iter().map(|b| b.backend.weight).sum();
        let mut target =
            (self.round_robin_index.fetch_add(1, Ordering::Relaxed) as u32) % total_weight;

        for backend in backends {
            if target < backend.backend.weight {
                return Some(backend);
            }
            target -= backend.backend.weight;
        }

        backends.first().copied()
    }

    /// Random selection
    fn select_random<'a>(
        &self,
        backends: &[&'a Arc<BackendState>],
    ) -> Option<&'a Arc<BackendState>> {
        use rand::Rng;
        let index = rand::thread_rng().gen_range(0..backends.len());
        backends.get(index).copied()
    }

    /// Add a backend
    pub fn add_backend(&self, backend: Backend) {
        let mut backends = self.backends.write();
        backends.push(Arc::new(BackendState::new(backend)));
    }

    /// Remove a backend
    pub fn remove_backend(&self, address: &str) {
        let mut backends = self.backends.write();
        backends.retain(|b| b.backend.address != address);
    }

    /// Get all backend health statuses
    pub fn health_statuses(&self) -> Vec<(Backend, HealthStatus)> {
        let backends = self.backends.read();
        backends
            .iter()
            .map(|b| (b.backend.clone(), b.get_status()))
            .collect()
    }

    /// Get backend count
    pub fn backend_count(&self) -> usize {
        self.backends.read().len()
    }

    /// Get healthy backend count
    pub fn healthy_backend_count(&self) -> usize {
        self.backends
            .read()
            .iter()
            .filter(|b| b.is_healthy())
            .count()
    }

    /// Mark backend as healthy/unhealthy
    pub fn set_backend_health(&self, address: &str, healthy: bool) {
        let backends = self.backends.read();
        if let Some(backend) = backends.iter().find(|b| b.backend.address == address) {
            if healthy {
                backend.mark_healthy();
            } else {
                backend.mark_unhealthy();
            }
        }
    }

    /// Start health check background task
    pub fn start_health_checks<F>(self: Arc<Self>, check_fn: F) -> tokio::task::JoinHandle<()>
    where
        F: Fn(&Backend) -> futures::future::BoxFuture<'static, bool> + Send + Sync + 'static,
    {
        let check_fn = Arc::new(check_fn);

        tokio::spawn(async move {
            if let Some(config) = &self.config.health_check {
                let unhealthy_threshold = config.unhealthy_threshold;
                let mut interval = tokio::time::interval(config.interval);
                loop {
                    interval.tick().await;

                    let backends = self.backends.read().clone();
                    for backend_state in backends {
                        let check_fn = Arc::clone(&check_fn);
                        let threshold = unhealthy_threshold;

                        tokio::spawn(async move {
                            let is_healthy = check_fn(&backend_state.backend).await;

                            if is_healthy {
                                backend_state.mark_healthy();
                            } else {
                                let failures =
                                    backend_state.consecutive_failures.load(Ordering::Acquire);
                                if failures >= threshold {
                                    backend_state.mark_unhealthy();
                                }
                            }
                        });
                    }
                }
            }
        })
    }
}

/// RAII guard for backend connections
pub struct BackendGuard {
    backend: Backend,
    state: Arc<BackendState>,
}

impl BackendGuard {
    /// Get the backend address
    pub fn address(&self) -> &str {
        &self.backend.address
    }

    /// Record request success
    pub fn record_success(&self) {
        self.state.record_request(true);
    }

    /// Record request failure
    pub fn record_failure(&self) {
        self.state.record_request(false);
    }
}

/// Automatic cleanup when BackendGuard is dropped
///
/// # Behavior
///
/// When a `BackendGuard` is dropped (goes out of scope), it:
/// 1. **Decrements** the backend's active connection counter atomically
/// 2. **Releases** the backend connection slot back to the pool
///
/// This ensures accurate connection tracking for load balancing decisions,
/// particularly for the `LeastConnections` strategy.
///
/// # Memory Ordering
///
/// Uses `Ordering::Relaxed` for the decrement operation because:
/// - The counter is only used for relative comparison (which backend has fewer connections)
/// - Exact synchronization with other memory operations is not required
/// - Lower overhead for high-frequency connection operations
///
/// **Note**: The load balancer reads this counter with `Ordering::Acquire` during
/// backend selection to ensure visibility of decrements from other threads.
///
/// # Thread Safety
///
/// This Drop implementation is:
/// - **Lock-free**: Uses only atomic operations, no blocking
/// - **Panic-safe**: Always executes, even during stack unwinding
/// - **Concurrent-safe**: Multiple guards can be dropped simultaneously from different threads
///
/// # Connection Tracking Lifecycle
///
/// 1. **Creation**: `LoadBalancer::select()` increments counter when guard is created
/// 2. **Usage**: Application uses the backend connection for request processing
/// 3. **Cleanup**: Guard drop automatically decrements counter when connection is released
///
/// # Example
///
/// ```no_run
/// use rust_network_system::pooling::{LoadBalancer, Backend, Strategy, LoadBalancerConfig};
///
/// # fn example() {
/// let backends = vec![
///     Backend::new("server1:8080"),
///     Backend::new("server2:8080"),
/// ];
///
/// let config = LoadBalancerConfig::new(Strategy::LeastConnections);
/// let lb = LoadBalancer::new(config, backends);
///
/// {
///     let guard = lb.select().unwrap();
///     // Use connection: guard.address()
///     // Process request...
///     guard.record_success();
/// } // Guard dropped here - connection counter automatically decremented
/// # }
/// ```
impl Drop for BackendGuard {
    fn drop(&mut self) {
        self.state
            .active_connections
            .fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backend_creation() {
        let backend = Backend::new("localhost:8080");
        assert_eq!(backend.address, "localhost:8080");
        assert_eq!(backend.weight, 1);

        let weighted = Backend::with_weight("localhost:8081", 5);
        assert_eq!(weighted.weight, 5);
    }

    #[test]
    fn test_round_robin() {
        let backends = vec![
            Backend::new("server1:8080"),
            Backend::new("server2:8080"),
            Backend::new("server3:8080"),
        ];

        let lb = LoadBalancer::round_robin(backends);

        let b1 = lb.select().unwrap();
        let b2 = lb.select().unwrap();
        let b3 = lb.select().unwrap();
        let b4 = lb.select().unwrap();

        assert_eq!(b1.address(), "server1:8080");
        assert_eq!(b2.address(), "server2:8080");
        assert_eq!(b3.address(), "server3:8080");
        assert_eq!(b4.address(), "server1:8080");
    }

    #[test]
    fn test_least_connections() {
        let backends = vec![Backend::new("server1:8080"), Backend::new("server2:8080")];

        let config = LoadBalancerConfig::new(Strategy::LeastConnections);
        let lb = LoadBalancer::new(config, backends);

        let _g1 = lb.select().unwrap(); // server1
        let g2 = lb.select().unwrap(); // server2 (fewer connections)

        assert_eq!(g2.address(), "server2:8080");
    }

    #[test]
    fn test_health_filtering() {
        let backends = vec![Backend::new("server1:8080"), Backend::new("server2:8080")];

        let lb = LoadBalancer::round_robin(backends);

        // Mark server1 as unhealthy
        lb.set_backend_health("server1:8080", false);

        // Should only select server2
        let b1 = lb.select().unwrap();
        let b2 = lb.select().unwrap();

        assert_eq!(b1.address(), "server2:8080");
        assert_eq!(b2.address(), "server2:8080");
    }

    #[test]
    fn test_add_remove_backend() {
        let backends = vec![Backend::new("server1:8080")];
        let lb = LoadBalancer::round_robin(backends);

        assert_eq!(lb.backend_count(), 1);

        lb.add_backend(Backend::new("server2:8080"));
        assert_eq!(lb.backend_count(), 2);

        lb.remove_backend("server1:8080");
        assert_eq!(lb.backend_count(), 1);
    }

    #[test]
    fn test_backend_guard() {
        let backends = vec![Backend::new("server1:8080")];
        let lb = LoadBalancer::round_robin(backends);

        {
            let guard = lb.select().unwrap();
            guard.record_success();
        }

        let statuses = lb.health_statuses();
        assert_eq!(statuses[0].1.total_requests, 1);
        assert_eq!(statuses[0].1.failed_requests, 0);
    }

    #[test]
    fn test_error_tracking() {
        let backends = vec![Backend::new("server1:8080")];
        let lb = LoadBalancer::round_robin(backends);

        let guard = lb.select().unwrap();
        guard.record_failure();
        guard.record_failure();

        let statuses = lb.health_statuses();
        assert_eq!(statuses[0].1.total_requests, 2);
        assert_eq!(statuses[0].1.failed_requests, 2);
        assert_eq!(statuses[0].1.error_rate(), 1.0);
    }

    #[test]
    fn test_healthy_backend_count() {
        let backends = vec![
            Backend::new("server1:8080"),
            Backend::new("server2:8080"),
            Backend::new("server3:8080"),
        ];

        let lb = LoadBalancer::round_robin(backends);

        assert_eq!(lb.healthy_backend_count(), 3);

        lb.set_backend_health("server1:8080", false);
        assert_eq!(lb.healthy_backend_count(), 2);

        lb.set_backend_health("server2:8080", false);
        assert_eq!(lb.healthy_backend_count(), 1);
    }
}
