//! Circuit Breaker pattern implementation for fault tolerance

use std::sync::Arc;
use std::time::{Duration, Instant};

/// Circuit breaker state
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    /// Circuit is closed, requests flow normally
    Closed,
    /// Circuit is open, requests are rejected
    Open,
    /// Circuit is half-open, testing if service recovered
    HalfOpen,
}

/// Circuit breaker configuration
#[derive(Debug, Clone)]
pub struct CircuitBreakerConfig {
    /// Number of failures before opening the circuit
    pub failure_threshold: u32,
    /// Number of successes to close circuit from half-open
    pub success_threshold: u32,
    /// Timeout before attempting recovery (half-open state)
    pub timeout: Duration,
    /// Window size for failure counting
    pub window_size: Duration,
}

impl Default for CircuitBreakerConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 5,
            success_threshold: 2,
            timeout: Duration::from_secs(60),
            window_size: Duration::from_secs(10),
        }
    }
}

impl CircuitBreakerConfig {
    /// Create a new circuit breaker configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Set failure threshold
    pub fn with_failure_threshold(mut self, threshold: u32) -> Self {
        self.failure_threshold = threshold;
        self
    }

    /// Set success threshold
    pub fn with_success_threshold(mut self, threshold: u32) -> Self {
        self.success_threshold = threshold;
        self
    }

    /// Set timeout duration
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set window size
    pub fn with_window_size(mut self, window_size: Duration) -> Self {
        self.window_size = window_size;
        self
    }
}

/// Internal state of the circuit breaker
struct CircuitBreakerState {
    state: CircuitState,
    failure_count: u32,
    success_count: u32,
    last_failure_time: Option<Instant>,
    opened_at: Option<Instant>,
}

/// Circuit Breaker for fault tolerance
pub struct CircuitBreaker {
    config: CircuitBreakerConfig,
    state: Arc<parking_lot::RwLock<CircuitBreakerState>>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with default configuration
    pub fn new() -> Self {
        Self::with_config(CircuitBreakerConfig::default())
    }

    /// Create a new circuit breaker with custom configuration
    pub fn with_config(config: CircuitBreakerConfig) -> Self {
        Self {
            config,
            state: Arc::new(parking_lot::RwLock::new(CircuitBreakerState {
                state: CircuitState::Closed,
                failure_count: 0,
                success_count: 0,
                last_failure_time: None,
                opened_at: None,
            })),
        }
    }

    /// Check if a request is allowed
    pub fn is_request_allowed(&self) -> bool {
        let mut state = self.state.write();

        match state.state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                // Check if timeout has elapsed
                if let Some(opened_at) = state.opened_at {
                    if opened_at.elapsed() >= self.config.timeout {
                        // Transition to half-open
                        state.state = CircuitState::HalfOpen;
                        state.success_count = 0;
                        state.failure_count = 0;
                        true
                    } else {
                        false
                    }
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Record a successful request
    pub fn record_success(&self) {
        let mut state = self.state.write();

        match state.state {
            CircuitState::Closed => {
                // Reset failure count on success
                state.failure_count = 0;
                state.last_failure_time = None;
            }
            CircuitState::HalfOpen => {
                state.success_count += 1;

                // If enough successes, close the circuit
                if state.success_count >= self.config.success_threshold {
                    state.state = CircuitState::Closed;
                    state.failure_count = 0;
                    state.success_count = 0;
                    state.opened_at = None;
                }
            }
            CircuitState::Open => {
                // Ignore successes in open state
            }
        }
    }

    /// Record a failed request
    pub fn record_failure(&self) {
        let mut state = self.state.write();
        let now = Instant::now();

        // Check if we should reset the failure count (window expired)
        if let Some(last_failure) = state.last_failure_time {
            if now.duration_since(last_failure) > self.config.window_size {
                state.failure_count = 0;
            }
        }

        state.last_failure_time = Some(now);

        match state.state {
            CircuitState::Closed => {
                state.failure_count += 1;

                // Open circuit if threshold exceeded
                if state.failure_count >= self.config.failure_threshold {
                    state.state = CircuitState::Open;
                    state.opened_at = Some(now);
                }
            }
            CircuitState::HalfOpen => {
                // Any failure in half-open reopens the circuit
                state.state = CircuitState::Open;
                state.opened_at = Some(now);
                state.failure_count = 1;
                state.success_count = 0;
            }
            CircuitState::Open => {
                // Already open, just track the failure
                state.failure_count += 1;
            }
        }
    }

    /// Get current state
    pub fn state(&self) -> CircuitState {
        self.state.read().state
    }

    /// Get failure count
    pub fn failure_count(&self) -> u32 {
        self.state.read().failure_count
    }

    /// Get success count (for half-open state)
    pub fn success_count(&self) -> u32 {
        self.state.read().success_count
    }

    /// Reset the circuit breaker to closed state
    pub fn reset(&self) {
        let mut state = self.state.write();
        state.state = CircuitState::Closed;
        state.failure_count = 0;
        state.success_count = 0;
        state.last_failure_time = None;
        state.opened_at = None;
    }

    /// Execute a function with circuit breaker protection
    pub async fn call<F, T, E>(&self, f: F) -> Result<T, CircuitBreakerError<E>>
    where
        F: std::future::Future<Output = Result<T, E>>,
    {
        if !self.is_request_allowed() {
            return Err(CircuitBreakerError::CircuitOpen);
        }

        match f.await {
            Ok(result) => {
                self.record_success();
                Ok(result)
            }
            Err(err) => {
                self.record_failure();
                Err(CircuitBreakerError::InnerError(err))
            }
        }
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new()
    }
}

/// Circuit breaker error
#[derive(Debug)]
pub enum CircuitBreakerError<E> {
    /// Circuit is open, request rejected
    CircuitOpen,
    /// Inner operation failed
    InnerError(E),
}

impl<E: std::fmt::Display> std::fmt::Display for CircuitBreakerError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CircuitBreakerError::CircuitOpen => write!(f, "Circuit breaker is open"),
            CircuitBreakerError::InnerError(e) => write!(f, "Operation failed: {}", e),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for CircuitBreakerError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            CircuitBreakerError::CircuitOpen => None,
            CircuitBreakerError::InnerError(e) => Some(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_state() {
        let cb = CircuitBreaker::new();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.is_request_allowed());
    }

    #[test]
    fn test_open_on_failures() {
        let config = CircuitBreakerConfig::new().with_failure_threshold(3);
        let cb = CircuitBreaker::with_config(config);

        // Record failures
        for _ in 0..3 {
            cb.record_failure();
        }

        assert_eq!(cb.state(), CircuitState::Open);
        assert!(!cb.is_request_allowed());
    }

    #[test]
    fn test_half_open_after_timeout() {
        let config = CircuitBreakerConfig::new()
            .with_failure_threshold(2)
            .with_timeout(Duration::from_millis(100));
        let cb = CircuitBreaker::with_config(config);

        // Open the circuit
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for timeout
        std::thread::sleep(Duration::from_millis(150));

        // Should allow request and transition to half-open
        assert!(cb.is_request_allowed());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_close_from_half_open() {
        let config = CircuitBreakerConfig::new()
            .with_failure_threshold(2)
            .with_success_threshold(2)
            .with_timeout(Duration::from_millis(100));
        let cb = CircuitBreaker::with_config(config);

        // Open the circuit
        cb.record_failure();
        cb.record_failure();

        // Wait and transition to half-open
        std::thread::sleep(Duration::from_millis(150));
        cb.is_request_allowed();

        // Record successes
        cb.record_success();
        cb.record_success();

        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[test]
    fn test_reopen_on_half_open_failure() {
        let config = CircuitBreakerConfig::new()
            .with_failure_threshold(2)
            .with_timeout(Duration::from_millis(100));
        let cb = CircuitBreaker::with_config(config);

        // Open the circuit
        cb.record_failure();
        cb.record_failure();

        // Wait and transition to half-open
        std::thread::sleep(Duration::from_millis(150));
        cb.is_request_allowed();
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Failure in half-open should reopen
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_window_reset() {
        let config = CircuitBreakerConfig::new()
            .with_failure_threshold(3)
            .with_window_size(Duration::from_millis(100));
        let cb = CircuitBreaker::with_config(config);

        // Record 2 failures
        cb.record_failure();
        cb.record_failure();

        // Wait for window to expire
        std::thread::sleep(Duration::from_millis(150));

        // This failure should start a new window
        cb.record_failure();

        // Should still be closed (only 1 failure in current window)
        assert_eq!(cb.state(), CircuitState::Closed);
    }

    #[tokio::test]
    async fn test_call_success() {
        let cb = CircuitBreaker::new();

        let result = cb.call(async { Ok::<i32, &str>(42) }).await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn test_call_failure() {
        let config = CircuitBreakerConfig::new().with_failure_threshold(1);
        let cb = CircuitBreaker::with_config(config);

        let result = cb.call(async { Err::<i32, &str>("error") }).await;

        assert!(result.is_err());
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[tokio::test]
    async fn test_call_circuit_open() {
        let config = CircuitBreakerConfig::new().with_failure_threshold(1);
        let cb = CircuitBreaker::with_config(config);

        // Open the circuit
        cb.record_failure();

        let result = cb.call(async { Ok::<i32, &str>(42) }).await;

        assert!(matches!(result, Err(CircuitBreakerError::CircuitOpen)));
    }
}
