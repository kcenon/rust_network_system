//! Rate limiting with token bucket algorithm

use std::sync::Arc;
use std::time::{Duration, Instant};
use thiserror::Error;

/// Rate limiter errors
#[derive(Debug, Error)]
pub enum RateLimiterError {
    /// Timeout waiting for tokens
    #[error("Timeout waiting for tokens")]
    Timeout,
}

/// Rate limiter configuration
#[derive(Debug, Clone)]
pub struct RateLimiterConfig {
    /// Maximum number of tokens (burst capacity)
    pub capacity: u64,
    /// Number of tokens to refill per interval
    pub refill_amount: u64,
    /// Refill interval
    pub refill_interval: Duration,
}

impl Default for RateLimiterConfig {
    fn default() -> Self {
        Self {
            capacity: 100,
            refill_amount: 10,
            refill_interval: Duration::from_secs(1),
        }
    }
}

impl RateLimiterConfig {
    /// Create a new rate limiter configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Set capacity (maximum tokens)
    pub fn with_capacity(mut self, capacity: u64) -> Self {
        self.capacity = capacity;
        self
    }

    /// Set refill amount
    pub fn with_refill_amount(mut self, amount: u64) -> Self {
        self.refill_amount = amount;
        self
    }

    /// Set refill interval
    pub fn with_refill_interval(mut self, interval: Duration) -> Self {
        self.refill_interval = interval;
        self
    }

    /// Create configuration for requests per second
    pub fn per_second(rate: u64) -> Self {
        Self {
            capacity: rate,
            refill_amount: rate,
            refill_interval: Duration::from_secs(1),
        }
    }

    /// Create configuration for requests per minute
    pub fn per_minute(rate: u64) -> Self {
        Self {
            capacity: rate,
            refill_amount: rate,
            refill_interval: Duration::from_secs(60),
        }
    }
}

/// Token bucket state
struct TokenBucket {
    tokens: f64,
    last_refill: Instant,
}

/// Rate limiter using token bucket algorithm
pub struct RateLimiter {
    config: RateLimiterConfig,
    bucket: Arc<parking_lot::Mutex<TokenBucket>>,
}

impl RateLimiter {
    /// Create a new rate limiter with default configuration
    pub fn new() -> Self {
        Self::with_config(RateLimiterConfig::default())
    }

    /// Create a new rate limiter with custom configuration
    pub fn with_config(config: RateLimiterConfig) -> Self {
        Self {
            config: config.clone(),
            bucket: Arc::new(parking_lot::Mutex::new(TokenBucket {
                tokens: config.capacity as f64,
                last_refill: Instant::now(),
            })),
        }
    }

    /// Create a rate limiter for N requests per second
    pub fn per_second(rate: u64) -> Self {
        Self::with_config(RateLimiterConfig::per_second(rate))
    }

    /// Create a rate limiter for N requests per minute
    pub fn per_minute(rate: u64) -> Self {
        Self::with_config(RateLimiterConfig::per_minute(rate))
    }

    /// Refill tokens based on elapsed time
    fn refill_tokens(&self, bucket: &mut TokenBucket) {
        let now = Instant::now();
        let elapsed = now.duration_since(bucket.last_refill);

        // Calculate how many refill intervals have passed
        let intervals = elapsed.as_secs_f64() / self.config.refill_interval.as_secs_f64();
        let tokens_to_add = intervals * self.config.refill_amount as f64;

        bucket.tokens = (bucket.tokens + tokens_to_add).min(self.config.capacity as f64);
        bucket.last_refill = now;
    }

    /// Try to acquire N tokens
    pub fn try_acquire(&self, tokens: u64) -> bool {
        let mut bucket = self.bucket.lock();
        self.refill_tokens(&mut bucket);

        if bucket.tokens >= tokens as f64 {
            bucket.tokens -= tokens as f64;
            true
        } else {
            false
        }
    }

    /// Try to acquire a single token
    pub fn try_acquire_one(&self) -> bool {
        self.try_acquire(1)
    }

    /// Wait until tokens are available (async) with timeout
    pub async fn acquire(&self, tokens: u64, timeout: Duration) -> Result<bool, RateLimiterError> {
        let deadline = tokio::time::Instant::now() + timeout;

        loop {
            if self.try_acquire(tokens) {
                return Ok(true);
            }

            // Check if we've exceeded the deadline
            if tokio::time::Instant::now() >= deadline {
                return Err(RateLimiterError::Timeout);
            }

            // Calculate wait time based on refill rate
            let wait_time = self.config.refill_interval / self.config.refill_amount as u32;

            // Sleep with timeout awareness
            let sleep_duration =
                wait_time.min(deadline.saturating_duration_since(tokio::time::Instant::now()));
            if !sleep_duration.is_zero() {
                tokio::time::sleep(sleep_duration).await;
            }
        }
    }

    /// Wait until a single token is available (async) with timeout
    pub async fn acquire_one(&self, timeout: Duration) -> Result<bool, RateLimiterError> {
        self.acquire(1, timeout).await
    }

    /// Get available tokens
    pub fn available_tokens(&self) -> u64 {
        let mut bucket = self.bucket.lock();
        self.refill_tokens(&mut bucket);
        bucket.tokens.floor() as u64
    }

    /// Reset the rate limiter to full capacity
    pub fn reset(&self) {
        let mut bucket = self.bucket.lock();
        bucket.tokens = self.config.capacity as f64;
        bucket.last_refill = Instant::now();
    }
}

impl Default for RateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

/// Multi-key rate limiter for per-client rate limiting
pub struct KeyedRateLimiter<K: std::hash::Hash + Eq> {
    config: RateLimiterConfig,
    limiters: Arc<parking_lot::RwLock<std::collections::HashMap<K, Arc<RateLimiter>>>>,
}

impl<K: std::hash::Hash + Eq + Clone> KeyedRateLimiter<K> {
    /// Create a new keyed rate limiter
    pub fn new(config: RateLimiterConfig) -> Self {
        Self {
            config,
            limiters: Arc::new(parking_lot::RwLock::new(std::collections::HashMap::new())),
        }
    }

    /// Try to acquire tokens for a specific key
    pub fn try_acquire(&self, key: &K, tokens: u64) -> bool {
        // Try to get existing limiter
        {
            let limiters = self.limiters.read();
            if let Some(limiter) = limiters.get(key) {
                return limiter.try_acquire(tokens);
            }
        }

        // Create new limiter if not exists
        let mut limiters = self.limiters.write();
        let limiter = limiters
            .entry(key.clone())
            .or_insert_with(|| Arc::new(RateLimiter::with_config(self.config.clone())));

        limiter.try_acquire(tokens)
    }

    /// Try to acquire a single token for a specific key
    pub fn try_acquire_one(&self, key: &K) -> bool {
        self.try_acquire(key, 1)
    }

    /// Wait until tokens are available for a specific key (async) with timeout
    pub async fn acquire(
        &self,
        key: &K,
        tokens: u64,
        timeout: Duration,
    ) -> Result<bool, RateLimiterError> {
        // Get or create limiter
        let limiter = {
            let mut limiters = self.limiters.write();
            limiters
                .entry(key.clone())
                .or_insert_with(|| Arc::new(RateLimiter::with_config(self.config.clone())))
                .clone()
        };

        limiter.acquire(tokens, timeout).await
    }

    /// Wait until a single token is available for a specific key (async) with timeout
    pub async fn acquire_one(&self, key: &K, timeout: Duration) -> Result<bool, RateLimiterError> {
        self.acquire(key, 1, timeout).await
    }

    /// Get number of tracked keys
    pub fn key_count(&self) -> usize {
        self.limiters.read().len()
    }

    /// Remove a key from tracking
    pub fn remove_key(&self, key: &K) {
        self.limiters.write().remove(key);
    }

    /// Clear all keys
    pub fn clear(&self) {
        self.limiters.write().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_capacity() {
        let limiter = RateLimiter::per_second(10);
        assert_eq!(limiter.available_tokens(), 10);
    }

    #[test]
    fn test_acquire_tokens() {
        let limiter = RateLimiter::per_second(10);

        assert!(limiter.try_acquire(5));
        assert_eq!(limiter.available_tokens(), 5);

        assert!(limiter.try_acquire(5));
        assert_eq!(limiter.available_tokens(), 0);

        // Should fail when no tokens available
        assert!(!limiter.try_acquire(1));
    }

    #[test]
    fn test_refill() {
        let config = RateLimiterConfig::new()
            .with_capacity(10)
            .with_refill_amount(10)
            .with_refill_interval(Duration::from_millis(100));

        let limiter = RateLimiter::with_config(config);

        // Consume all tokens
        assert!(limiter.try_acquire(10));
        assert_eq!(limiter.available_tokens(), 0);

        // Wait for refill
        std::thread::sleep(Duration::from_millis(150));

        // Should have refilled
        assert!(limiter.available_tokens() > 0);
    }

    #[test]
    fn test_per_second() {
        let limiter = RateLimiter::per_second(100);
        assert_eq!(limiter.available_tokens(), 100);
    }

    #[test]
    fn test_per_minute() {
        let limiter = RateLimiter::per_minute(60);
        assert_eq!(limiter.available_tokens(), 60);
    }

    #[tokio::test]
    async fn test_async_acquire() {
        let config = RateLimiterConfig::new()
            .with_capacity(2)
            .with_refill_amount(2)
            .with_refill_interval(Duration::from_millis(100));

        let limiter = RateLimiter::with_config(config);

        // Acquire all tokens
        assert!(limiter.try_acquire(2));

        // This should wait and succeed
        let start = Instant::now();
        assert!(limiter.acquire_one(Duration::from_secs(5)).await.unwrap());
        let elapsed = start.elapsed();

        assert!(elapsed >= Duration::from_millis(50)); // Should have waited
    }

    #[tokio::test]
    async fn test_async_acquire_timeout() {
        let config = RateLimiterConfig::new()
            .with_capacity(1)
            .with_refill_amount(1)
            .with_refill_interval(Duration::from_secs(60)); // Very slow refill

        let limiter = RateLimiter::with_config(config);

        // Acquire the only token
        assert!(limiter.try_acquire(1));

        // This should timeout since refill takes 60 seconds
        let result = limiter.acquire_one(Duration::from_millis(100)).await;
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), RateLimiterError::Timeout));
    }

    #[test]
    fn test_keyed_limiter() {
        let config = RateLimiterConfig::per_second(5);
        let limiter = KeyedRateLimiter::new(config);

        // Different keys should have independent limits
        assert!(limiter.try_acquire(&"user1", 5));
        assert!(limiter.try_acquire(&"user2", 5));

        // Same key should be limited
        assert!(!limiter.try_acquire(&"user1", 1));
        assert!(!limiter.try_acquire(&"user2", 1));

        assert_eq!(limiter.key_count(), 2);
    }

    #[test]
    fn test_keyed_limiter_remove() {
        let config = RateLimiterConfig::per_second(5);
        let limiter = KeyedRateLimiter::new(config);

        limiter.try_acquire(&"user1", 1);
        limiter.try_acquire(&"user2", 1);

        assert_eq!(limiter.key_count(), 2);

        limiter.remove_key(&"user1");
        assert_eq!(limiter.key_count(), 1);
    }

    #[test]
    fn test_reset() {
        let limiter = RateLimiter::per_second(10);

        limiter.try_acquire(10);
        assert_eq!(limiter.available_tokens(), 0);

        limiter.reset();
        assert_eq!(limiter.available_tokens(), 10);
    }
}
