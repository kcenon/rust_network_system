//! Multi-layer caching system with LRU and TTL support

use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Cache configuration
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// Maximum number of entries
    pub capacity: usize,
    /// Time-to-live for cache entries
    pub ttl: Option<Duration>,
    /// Enable LRU eviction
    pub enable_lru: bool,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            capacity: 1000,
            ttl: Some(Duration::from_secs(300)),
            enable_lru: true,
        }
    }
}

impl CacheConfig {
    /// Create a new cache configuration
    pub fn new() -> Self {
        Self::default()
    }

    /// Set capacity
    pub fn with_capacity(mut self, capacity: usize) -> Self {
        self.capacity = capacity;
        self
    }

    /// Set TTL
    pub fn with_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.ttl = ttl;
        self
    }

    /// Enable or disable LRU
    pub fn with_lru(mut self, enable: bool) -> Self {
        self.enable_lru = enable;
        self
    }
}

/// Cache entry with metadata
struct CacheEntry<V> {
    value: V,
    created_at: Instant,
    last_accessed: Instant,
    access_count: u64,
}

impl<V> CacheEntry<V> {
    fn new(value: V) -> Self {
        let now = Instant::now();
        Self {
            value,
            created_at: now,
            last_accessed: now,
            access_count: 0,
        }
    }

    fn is_expired(&self, ttl: Duration) -> bool {
        self.created_at.elapsed() > ttl
    }

    fn touch(&mut self) {
        self.last_accessed = Instant::now();
        self.access_count += 1;
    }
}

/// LRU cache with TTL support
pub struct Cache<K: Hash + Eq + Clone, V: Clone> {
    config: CacheConfig,
    entries: Arc<parking_lot::RwLock<HashMap<K, CacheEntry<V>>>>,
    lru_order: Arc<parking_lot::Mutex<VecDeque<K>>>,
}

impl<K: Hash + Eq + Clone, V: Clone> Cache<K, V> {
    /// Create a new cache
    pub fn new(config: CacheConfig) -> Self {
        Self {
            config,
            entries: Arc::new(parking_lot::RwLock::new(HashMap::new())),
            lru_order: Arc::new(parking_lot::Mutex::new(VecDeque::new())),
        }
    }

    /// Create a cache with default configuration
    pub fn with_default_config() -> Self {
        Self::new(CacheConfig::default())
    }

    /// Get a value from the cache
    pub fn get(&self, key: &K) -> Option<V> {
        let mut entries = self.entries.write();

        if let Some(entry) = entries.get_mut(key) {
            // Check if expired
            if let Some(ttl) = self.config.ttl {
                if entry.is_expired(ttl) {
                    entries.remove(key);
                    return None;
                }
            }

            entry.touch();

            // Update LRU order
            if self.config.enable_lru {
                let mut lru = self.lru_order.lock();
                if let Some(pos) = lru.iter().position(|k| k == key) {
                    lru.remove(pos);
                    lru.push_back(key.clone());
                }
            }

            Some(entry.value.clone())
        } else {
            None
        }
    }

    /// Insert a value into the cache
    pub fn insert(&self, key: K, value: V) {
        let mut entries = self.entries.write();

        // Check if we need to evict
        if entries.len() >= self.config.capacity && !entries.contains_key(&key) {
            self.evict_one(&mut entries);
        }

        // Insert new entry
        entries.insert(key.clone(), CacheEntry::new(value));

        // Update LRU order
        if self.config.enable_lru {
            let mut lru = self.lru_order.lock();
            lru.push_back(key);
        }
    }

    /// Remove a value from the cache
    pub fn remove(&self, key: &K) -> Option<V> {
        let mut entries = self.entries.write();
        let removed = entries.remove(key).map(|entry| entry.value);

        if removed.is_some() && self.config.enable_lru {
            let mut lru = self.lru_order.lock();
            if let Some(pos) = lru.iter().position(|k| k == key) {
                lru.remove(pos);
            }
        }

        removed
    }

    /// Clear all entries
    pub fn clear(&self) {
        self.entries.write().clear();
        if self.config.enable_lru {
            self.lru_order.lock().clear();
        }
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        let entries = self.entries.read();
        let total_accesses: u64 = entries.values().map(|e| e.access_count).sum();

        CacheStats {
            size: entries.len(),
            capacity: self.config.capacity,
            total_accesses,
        }
    }

    /// Prune expired entries
    pub fn prune_expired(&self) -> usize {
        if let Some(ttl) = self.config.ttl {
            let mut entries = self.entries.write();
            let expired_keys: Vec<K> = entries
                .iter()
                .filter(|(_, entry)| entry.is_expired(ttl))
                .map(|(key, _)| key.clone())
                .collect();

            let count = expired_keys.len();

            for key in expired_keys {
                entries.remove(&key);

                if self.config.enable_lru {
                    let mut lru = self.lru_order.lock();
                    if let Some(pos) = lru.iter().position(|k| k == &key) {
                        lru.remove(pos);
                    }
                }
            }

            count
        } else {
            0
        }
    }

    /// Evict one entry (LRU)
    fn evict_one(&self, entries: &mut HashMap<K, CacheEntry<V>>) {
        if self.config.enable_lru {
            let mut lru = self.lru_order.lock();
            if let Some(key) = lru.pop_front() {
                entries.remove(&key);
            }
        } else {
            // If LRU is disabled, just remove the first entry
            if let Some(key) = entries.keys().next().cloned() {
                entries.remove(&key);
            }
        }
    }

    /// Get or compute a value
    pub fn get_or_insert_with<F>(&self, key: K, f: F) -> V
    where
        F: FnOnce() -> V,
    {
        if let Some(value) = self.get(&key) {
            return value;
        }

        let value = f();
        self.insert(key, value.clone());
        value
    }

    /// Start background cleanup task
    pub fn start_cleanup(self: Arc<Self>, interval: Duration) -> tokio::task::JoinHandle<()>
    where
        K: Send + Sync + 'static,
        V: Send + Sync + 'static,
    {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(interval);
            loop {
                interval.tick().await;
                self.prune_expired();
            }
        })
    }
}

impl<K: Hash + Eq + Clone, V: Clone> Clone for Cache<K, V> {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            entries: Arc::clone(&self.entries),
            lru_order: Arc::clone(&self.lru_order),
        }
    }
}

/// Cache statistics
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Current number of entries
    pub size: usize,
    /// Maximum capacity
    pub capacity: usize,
    /// Total number of accesses
    pub total_accesses: u64,
}

impl CacheStats {
    /// Calculate cache hit rate (requires external hit/miss tracking)
    pub fn usage_percentage(&self) -> f64 {
        (self.size as f64 / self.capacity as f64) * 100.0
    }
}

/// Two-tier cache (L1 memory, L2 could be Redis/disk)
pub struct TieredCache<K: Hash + Eq + Clone, V: Clone> {
    l1: Cache<K, V>,
    #[allow(dead_code)] // Reserved for future L2 cache implementation
    l1_capacity: usize,
}

impl<K: Hash + Eq + Clone, V: Clone> TieredCache<K, V> {
    /// Create a new tiered cache
    pub fn new(l1_capacity: usize, l1_ttl: Option<Duration>) -> Self {
        let l1_config = CacheConfig::new()
            .with_capacity(l1_capacity)
            .with_ttl(l1_ttl);

        Self {
            l1: Cache::new(l1_config),
            l1_capacity,
        }
    }

    /// Get from L1 cache
    pub fn get(&self, key: &K) -> Option<V> {
        self.l1.get(key)
    }

    /// Insert into L1 cache
    pub fn insert(&self, key: K, value: V) {
        self.l1.insert(key, value);
    }

    /// Get L1 statistics
    pub fn stats(&self) -> CacheStats {
        self.l1.stats()
    }

    /// Clear all caches
    pub fn clear(&self) {
        self.l1.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_insert_get() {
        let cache = Cache::<String, i32>::with_default_config();

        cache.insert("key1".to_string(), 42);
        assert_eq!(cache.get(&"key1".to_string()), Some(42));
        assert_eq!(cache.get(&"key2".to_string()), None);
    }

    #[test]
    fn test_cache_capacity() {
        let config = CacheConfig::new().with_capacity(2);
        let cache = Cache::<String, i32>::new(config);

        cache.insert("key1".to_string(), 1);
        cache.insert("key2".to_string(), 2);
        cache.insert("key3".to_string(), 3);

        // key1 should be evicted (LRU)
        assert_eq!(cache.get(&"key1".to_string()), None);
        assert_eq!(cache.get(&"key2".to_string()), Some(2));
        assert_eq!(cache.get(&"key3".to_string()), Some(3));
    }

    #[test]
    fn test_lru_order() {
        let config = CacheConfig::new().with_capacity(2);
        let cache = Cache::<String, i32>::new(config);

        cache.insert("key1".to_string(), 1);
        cache.insert("key2".to_string(), 2);

        // Access key1 to make it recently used
        cache.get(&"key1".to_string());

        // Insert key3, key2 should be evicted
        cache.insert("key3".to_string(), 3);

        assert_eq!(cache.get(&"key1".to_string()), Some(1));
        assert_eq!(cache.get(&"key2".to_string()), None);
        assert_eq!(cache.get(&"key3".to_string()), Some(3));
    }

    #[test]
    fn test_ttl_expiration() {
        let config = CacheConfig::new().with_ttl(Some(Duration::from_millis(50)));
        let cache = Cache::<String, i32>::new(config);

        cache.insert("key1".to_string(), 42);
        assert_eq!(cache.get(&"key1".to_string()), Some(42));

        // Wait for expiration
        std::thread::sleep(Duration::from_millis(100));

        assert_eq!(cache.get(&"key1".to_string()), None);
    }

    #[test]
    fn test_prune_expired() {
        let config = CacheConfig::new().with_ttl(Some(Duration::from_millis(50)));
        let cache = Cache::<String, i32>::new(config);

        cache.insert("key1".to_string(), 1);
        cache.insert("key2".to_string(), 2);

        std::thread::sleep(Duration::from_millis(100));

        let pruned = cache.prune_expired();
        assert_eq!(pruned, 2);

        let stats = cache.stats();
        assert_eq!(stats.size, 0);
    }

    #[test]
    fn test_get_or_insert_with() {
        let cache = Cache::<String, i32>::with_default_config();

        let value1 = cache.get_or_insert_with("key1".to_string(), || 42);
        assert_eq!(value1, 42);

        let value2 = cache.get_or_insert_with("key1".to_string(), || 100);
        assert_eq!(value2, 42); // Should return cached value
    }

    #[test]
    fn test_cache_stats() {
        let cache = Cache::<String, i32>::with_default_config();

        cache.insert("key1".to_string(), 1);
        cache.insert("key2".to_string(), 2);

        cache.get(&"key1".to_string());
        cache.get(&"key1".to_string());
        cache.get(&"key2".to_string());

        let stats = cache.stats();
        assert_eq!(stats.size, 2);
        assert_eq!(stats.total_accesses, 3);
    }

    #[test]
    fn test_tiered_cache() {
        let cache = TieredCache::<String, i32>::new(10, Some(Duration::from_secs(60)));

        cache.insert("key1".to_string(), 42);
        assert_eq!(cache.get(&"key1".to_string()), Some(42));

        let stats = cache.stats();
        assert_eq!(stats.size, 1);
    }

    #[test]
    fn test_remove() {
        let cache = Cache::<String, i32>::with_default_config();

        cache.insert("key1".to_string(), 42);
        assert_eq!(cache.get(&"key1".to_string()), Some(42));

        let removed = cache.remove(&"key1".to_string());
        assert_eq!(removed, Some(42));
        assert_eq!(cache.get(&"key1".to_string()), None);
    }

    #[test]
    fn test_clear() {
        let cache = Cache::<String, i32>::with_default_config();

        cache.insert("key1".to_string(), 1);
        cache.insert("key2".to_string(), 2);

        cache.clear();

        let stats = cache.stats();
        assert_eq!(stats.size, 0);
    }
}
