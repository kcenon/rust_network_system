//! Stability patterns for fault tolerance and resilience

pub mod circuit_breaker;
pub mod graceful_shutdown;
pub mod rate_limiter;

pub use circuit_breaker::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError, CircuitState,
};
pub use graceful_shutdown::{GracefulShutdown, ShutdownCoordinator, ShutdownGuard, ShutdownSignal};
pub use rate_limiter::{KeyedRateLimiter, RateLimiter, RateLimiterConfig};
