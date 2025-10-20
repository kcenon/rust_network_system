//! Resource pooling and optimization

pub mod cache;
pub mod connection_pool;
pub mod load_balancer;

pub use cache::{Cache, CacheConfig, CacheStats, TieredCache};
pub use connection_pool::{
    ConnectionPool, PoolConfig, PoolError, PoolStats, Poolable, PooledGuard,
};
pub use load_balancer::{
    Backend, BackendGuard, HealthCheckConfig, HealthStatus, LoadBalancer, LoadBalancerConfig,
    Strategy,
};
