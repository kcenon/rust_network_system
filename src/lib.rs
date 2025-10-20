//! Rust Network System
//!
//! A production-ready, high-performance async networking framework for Rust.
//!
//! ## Features
//!
//! - Async TCP client/server built on tokio
//! - Session management with automatic connection handling
//! - Length-prefixed message protocol
//! - JSON serialization support
//! - Configurable timeouts and keep-alive
//! - Connection pooling and limits
//! - Integration with logging and monitoring systems
//!
//! ## Example
//!
//! ```no_run
//! use rust_network_system::{TcpServer, ServerConfig, DefaultSessionHandler};
//! use std::sync::Arc;
//!
//! #[tokio::main]
//! async fn main() -> Result<(), Box<dyn std::error::Error>> {
//!     let config = ServerConfig::new("127.0.0.1:8080");
//!     let mut server = TcpServer::with_config(config);
//!
//!     let handler = Arc::new(DefaultSessionHandler);
//!     server.start(handler).await?;
//!
//!     Ok(())
//! }
//! ```

pub mod core;
pub mod error;
pub mod pooling;
pub mod session;
pub mod stability;
pub mod tracing;

#[cfg(feature = "integration")]
pub mod integration;

#[cfg(feature = "tls")]
pub mod tls;

// Re-export main types
pub use core::{ClientConfig, Message, ServerConfig, TcpClient, TcpServer, MAX_MESSAGE_SIZE};
pub use error::{NetworkError, Result};
pub use pooling::{
    Backend, BackendGuard, Cache, CacheConfig, ConnectionPool, HealthCheckConfig, LoadBalancer,
    LoadBalancerConfig, PoolConfig, PoolError, Poolable, Strategy, TieredCache,
};
pub use session::{DefaultSessionHandler, Session, SessionHandler, SessionState};
pub use stability::{
    CircuitBreaker, CircuitBreakerConfig, CircuitBreakerError, CircuitState, GracefulShutdown,
    KeyedRateLimiter, RateLimiter, RateLimiterConfig, ShutdownCoordinator, ShutdownSignal,
};
pub use tracing::{Span, SpanCollector, TraceContext};

#[cfg(feature = "tls")]
pub use tls::TlsConfig;
