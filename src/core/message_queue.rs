//! Message queue for buffering outbound messages
//!
//! Provides asynchronous message queuing to handle backpressure and prevent
//! message loss when the network is slow or temporarily unavailable.

use crate::core::message::Message;
use crate::error::{NetworkError, Result};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, Semaphore};
use tokio::time::{timeout, Duration};

/// Configuration for message queue
#[derive(Debug, Clone)]
pub struct MessageQueueConfig {
    /// Maximum number of messages in queue
    pub max_size: usize,
    /// Timeout for enqueue operation when queue is full
    pub enqueue_timeout: Duration,
    /// Enable message dropping when queue is full (vs blocking)
    pub drop_on_full: bool,
}

impl Default for MessageQueueConfig {
    fn default() -> Self {
        Self {
            max_size: 1000,
            enqueue_timeout: Duration::from_secs(5),
            drop_on_full: false,
        }
    }
}

impl MessageQueueConfig {
    /// Create a new configuration
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set maximum queue size
    #[must_use]
    pub fn with_max_size(mut self, size: usize) -> Self {
        self.max_size = size;
        self
    }

    /// Set enqueue timeout
    #[must_use]
    pub fn with_enqueue_timeout(mut self, timeout: Duration) -> Self {
        self.enqueue_timeout = timeout;
        self
    }

    /// Enable message dropping when queue is full
    #[must_use]
    pub fn with_drop_on_full(mut self, drop: bool) -> Self {
        self.drop_on_full = drop;
        self
    }
}

/// Async message queue for buffering outbound messages
///
/// # Features
///
/// - **Bounded capacity**: Prevents unbounded memory growth
/// - **Backpressure handling**: Configurable blocking or dropping behavior
/// - **Statistics tracking**: Queue depth, enqueued/dequeued counts, drops
/// - **Non-blocking dequeue**: Try-dequeue for polling usage patterns
///
/// # Example
///
/// ```no_run
/// use rust_network_system::core::{Message, MessageQueue, MessageQueueConfig};
///
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// let config = MessageQueueConfig::new()
///     .with_max_size(100)
///     .with_drop_on_full(false);
///
/// let queue = MessageQueue::new(config);
///
/// // Enqueue message
/// let msg = Message::new("Hello");
/// queue.enqueue(msg).await?;
///
/// // Dequeue message
/// if let Some(msg) = queue.dequeue().await {
///     println!("Received: {}", String::from_utf8_lossy(msg.data()));
/// }
///
/// // Check stats
/// let stats = queue.stats();
/// println!("Queue depth: {}", stats.depth);
/// println!("Total enqueued: {}", stats.enqueued);
/// println!("Dropped messages: {}", stats.dropped);
/// # Ok(())
/// # }
/// ```
pub struct MessageQueue {
    config: MessageQueueConfig,
    tx: mpsc::Sender<Message>,
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<Message>>>,
    semaphore: Arc<Semaphore>,
    enqueued_count: Arc<AtomicU64>,
    dequeued_count: Arc<AtomicU64>,
    dropped_count: Arc<AtomicU64>,
}

impl MessageQueue {
    /// Create a new message queue
    pub fn new(config: MessageQueueConfig) -> Self {
        let (tx, rx) = mpsc::channel(config.max_size);
        let semaphore = Arc::new(Semaphore::new(config.max_size));

        Self {
            config,
            tx,
            rx: Arc::new(tokio::sync::Mutex::new(rx)),
            semaphore,
            enqueued_count: Arc::new(AtomicU64::new(0)),
            dequeued_count: Arc::new(AtomicU64::new(0)),
            dropped_count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Create a queue with default configuration
    #[must_use]
    pub fn with_default_config() -> Self {
        Self::new(MessageQueueConfig::default())
    }

    /// Enqueue a message with backpressure handling
    ///
    /// # Backpressure Behavior
    ///
    /// - If `drop_on_full` is true: Drops message immediately when queue is full
    /// - If `drop_on_full` is false: Waits up to `enqueue_timeout` for space
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Queue is full and `drop_on_full` is true (message dropped)
    /// - Queue is full and enqueue timeout expires
    /// - Queue is closed
    pub async fn enqueue(&self, message: Message) -> Result<()> {
        // Try to acquire permit (indicates queue has space)
        let permit = if self.config.drop_on_full {
            // Non-blocking: drop message immediately if queue is full
            self.semaphore.try_acquire().map_err(|_| {
                self.dropped_count.fetch_add(1, Ordering::Relaxed);
                NetworkError::other("Message queue full, message dropped")
            })?
        } else {
            // Blocking with timeout: wait for space in queue
            timeout(self.config.enqueue_timeout, self.semaphore.acquire())
                .await
                .map_err(|_| NetworkError::timeout("Enqueue timeout: queue full"))?
                .map_err(|_| NetworkError::other("Queue semaphore closed"))?
        };

        // Send message to queue
        self.tx
            .send(message)
            .await
            .map_err(|_| NetworkError::other("Message queue closed"))?;

        self.enqueued_count.fetch_add(1, Ordering::Relaxed);

        // Forget permit to keep it acquired until dequeue
        permit.forget();

        Ok(())
    }

    /// Try to enqueue without blocking
    ///
    /// Returns immediately with error if queue is full.
    ///
    /// # Example
    ///
    /// ```no_run
    /// # use rust_network_system::core::{Message, MessageQueue};
    /// # async fn example(queue: &MessageQueue) -> Result<(), Box<dyn std::error::Error>> {
    /// let msg = Message::new("test");
    /// match queue.try_enqueue(msg).await {
    ///     Ok(()) => println!("Enqueued"),
    ///     Err(e) => println!("Queue full: {}", e),
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn try_enqueue(&self, message: Message) -> Result<()> {
        // Try to acquire permit without blocking
        let permit = self.semaphore.try_acquire().map_err(|_| {
            self.dropped_count.fetch_add(1, Ordering::Relaxed);
            NetworkError::other("Message queue full")
        })?;

        // Send message to queue
        self.tx
            .send(message)
            .await
            .map_err(|_| NetworkError::other("Message queue closed"))?;

        self.enqueued_count.fetch_add(1, Ordering::Relaxed);

        // Forget permit to keep it acquired until dequeue
        permit.forget();

        Ok(())
    }

    /// Dequeue a message (blocking)
    ///
    /// Waits for a message to become available. Returns None if queue is closed.
    pub async fn dequeue(&self) -> Option<Message> {
        let mut rx_guard = self.rx.lock().await;
        let message = rx_guard.recv().await;

        if message.is_some() {
            self.dequeued_count.fetch_add(1, Ordering::Relaxed);
            // Release one permit to allow next enqueue
            self.semaphore.add_permits(1);
        }

        message
    }

    /// Try to dequeue without blocking
    ///
    /// Returns immediately with None if queue is empty or if the lock cannot
    /// be acquired immediately.
    pub fn try_dequeue(&self) -> Option<Message> {
        // Try to acquire lock without blocking
        let mut rx_guard = self.rx.try_lock().ok()?;

        // Try to receive message without blocking
        let message = rx_guard.try_recv().ok();

        if message.is_some() {
            self.dequeued_count.fetch_add(1, Ordering::Relaxed);
            // Release one permit to allow next enqueue
            self.semaphore.add_permits(1);
        }

        message
    }

    /// Get queue statistics
    #[must_use]
    pub fn stats(&self) -> MessageQueueStats {
        let enqueued = self.enqueued_count.load(Ordering::Relaxed);
        let dequeued = self.dequeued_count.load(Ordering::Relaxed);
        let depth = enqueued.saturating_sub(dequeued);

        MessageQueueStats {
            depth,
            max_size: self.config.max_size as u64,
            enqueued,
            dequeued,
            dropped: self.dropped_count.load(Ordering::Relaxed),
        }
    }

    /// Check if queue is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.stats().depth == 0
    }

    /// Check if queue is full
    #[must_use]
    pub fn is_full(&self) -> bool {
        self.semaphore.available_permits() == 0
    }

    /// Get current queue depth
    #[must_use]
    pub fn depth(&self) -> u64 {
        self.stats().depth
    }
}

/// Statistics for message queue
#[derive(Debug, Clone)]
pub struct MessageQueueStats {
    /// Current number of messages in queue
    pub depth: u64,
    /// Maximum queue size
    pub max_size: u64,
    /// Total messages enqueued
    pub enqueued: u64,
    /// Total messages dequeued
    pub dequeued: u64,
    /// Total messages dropped (when queue was full)
    pub dropped: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_message_queue_basic() {
        let queue = MessageQueue::with_default_config();

        // Enqueue message
        let msg = Message::new("test");
        queue.enqueue(msg).await.unwrap();

        // Dequeue message
        let received = queue.dequeue().await.unwrap();
        assert_eq!(String::from_utf8_lossy(received.data()), "test");

        // Check stats
        let stats = queue.stats();
        assert_eq!(stats.enqueued, 1);
        assert_eq!(stats.dequeued, 1);
        assert_eq!(stats.dropped, 0);
    }

    #[tokio::test]
    async fn test_message_queue_backpressure() {
        let config = MessageQueueConfig::new()
            .with_max_size(2)
            .with_drop_on_full(false);
        let queue = MessageQueue::new(config);

        // Fill queue
        queue.enqueue(Message::new("msg1")).await.unwrap();
        queue.enqueue(Message::new("msg2")).await.unwrap();

        // Queue should be full
        assert!(queue.is_full());

        // Try enqueue should fail immediately
        let result = queue.try_enqueue(Message::new("msg3")).await;
        assert!(result.is_err());

        // Check dropped count
        let stats = queue.stats();
        assert_eq!(stats.dropped, 1);
    }

    #[tokio::test]
    async fn test_message_queue_drop_on_full() {
        let config = MessageQueueConfig::new()
            .with_max_size(1)
            .with_drop_on_full(true);
        let queue = MessageQueue::new(config);

        // Fill queue
        queue.enqueue(Message::new("msg1")).await.unwrap();

        // Next enqueue should drop immediately
        let result = queue.enqueue(Message::new("msg2")).await;
        assert!(result.is_err());

        let stats = queue.stats();
        assert_eq!(stats.dropped, 1);
        assert_eq!(stats.enqueued, 1);
    }

    #[tokio::test]
    async fn test_message_queue_try_dequeue() {
        let queue = MessageQueue::with_default_config();

        // Try dequeue from empty queue
        assert!(queue.try_dequeue().is_none());

        // Enqueue and try dequeue
        queue.enqueue(Message::new("test")).await.unwrap();
        let msg = queue.try_dequeue().unwrap();
        assert_eq!(String::from_utf8_lossy(msg.data()), "test");

        // Queue should be empty again
        assert!(queue.is_empty());
    }

    #[tokio::test]
    async fn test_message_queue_concurrent() {
        let queue = Arc::new(MessageQueue::with_default_config());
        let queue_clone = Arc::clone(&queue);

        // Spawn producer
        let producer = tokio::spawn(async move {
            for i in 0..100 {
                let msg = Message::new(format!("msg{}", i));
                queue_clone.enqueue(msg).await.unwrap();
            }
        });

        // Spawn consumer
        let consumer = tokio::spawn(async move {
            let mut count = 0;
            while count < 100 {
                if queue.dequeue().await.is_some() {
                    count += 1;
                }
            }
            count
        });

        // Wait for completion
        producer.await.unwrap();
        let consumed = consumer.await.unwrap();

        assert_eq!(consumed, 100);
    }
}
