//! Thread pool integration for CPU-intensive and blocking operations

use crate::core::message::Message;
use crate::error::{NetworkError, Result};
use crate::session::{Session, SessionHandler};
use async_trait::async_trait;
use rust_thread_system::{ThreadPool, ThreadPoolConfig};
use std::sync::Arc;
use tokio::sync::oneshot;

/// Message processor that can handle CPU-intensive operations
#[async_trait]
pub trait MessageProcessor: Send + Sync {
    /// Process a message in a blocking or CPU-intensive way
    fn process(&self, message: Message) -> Result<Message>;
}

/// Session handler that delegates CPU-intensive work to thread pool
pub struct ThreadPoolSessionHandler<H, P>
where
    H: SessionHandler + Send + Sync,
    P: MessageProcessor,
{
    inner: Arc<H>,
    thread_pool: Arc<ThreadPool>,
    processor: Arc<P>,
}

impl<H, P> ThreadPoolSessionHandler<H, P>
where
    H: SessionHandler + Send + Sync + 'static,
    P: MessageProcessor + 'static,
{
    /// Create a new thread pool session handler
    ///
    /// # Errors
    ///
    /// Returns error if thread pool creation fails
    pub fn new(inner: Arc<H>, processor: Arc<P>, pool_size: usize) -> Result<Self> {
        let config = ThreadPoolConfig::new(pool_size);
        let mut thread_pool = ThreadPool::with_config(config)
            .map_err(|e| NetworkError::other(format!("Failed to create thread pool: {}", e)))?;

        thread_pool
            .start()
            .map_err(|e| NetworkError::other(format!("Failed to start thread pool: {}", e)))?;

        Ok(Self {
            inner,
            thread_pool: Arc::new(thread_pool),
            processor,
        })
    }

    /// Create with existing thread pool
    #[must_use]
    pub fn with_pool(inner: Arc<H>, processor: Arc<P>, thread_pool: Arc<ThreadPool>) -> Self {
        Self {
            inner,
            thread_pool,
            processor,
        }
    }

    /// Get the underlying thread pool
    #[must_use]
    pub fn thread_pool(&self) -> &Arc<ThreadPool> {
        &self.thread_pool
    }
}

#[async_trait]
impl<H, P> SessionHandler for ThreadPoolSessionHandler<H, P>
where
    H: SessionHandler + Send + Sync + 'static,
    P: MessageProcessor + 'static,
{
    async fn on_connected(&self, session: &Session) {
        self.inner.on_connected(session).await;
    }

    async fn on_message(&self, session: &Session, message: Message) {
        let processor = Arc::clone(&self.processor);
        let session_clone = Arc::new(SessionRef {
            id: session.id(),
            peer_addr: session.peer_addr(),
        });

        // Create channel for result
        let (tx, rx) = oneshot::channel();

        // Submit to thread pool
        let job_result = self.thread_pool.execute(move || {
            // Process message in thread pool (CPU-intensive or blocking)
            let result = processor.process(message);
            let _ = tx.send(result);
            Ok(())
        });

        if let Err(e) = job_result {
            tracing::error!(
                "Failed to submit job to thread pool for session {}: {}",
                session.id(),
                e
            );
            return;
        }

        // Wait for processing result
        match rx.await {
            Ok(Ok(processed_message)) => {
                // Delegate processed message to inner handler
                self.inner.on_message(session, processed_message).await;
            }
            Ok(Err(e)) => {
                tracing::error!(
                    "Message processing failed for session {}: {}",
                    session_clone.id,
                    e
                );
            }
            Err(_) => {
                tracing::error!("Thread pool job cancelled for session {}", session_clone.id);
            }
        }
    }

    async fn on_disconnected(&self, session: &Session) {
        self.inner.on_disconnected(session).await;
    }
}

/// Minimal session reference for thread pool jobs
struct SessionRef {
    id: u128,
    #[allow(dead_code)]
    peer_addr: std::net::SocketAddr,
}

/// Identity processor that returns message unchanged (for testing)
pub struct IdentityProcessor;

impl MessageProcessor for IdentityProcessor {
    fn process(&self, message: Message) -> Result<Message> {
        Ok(message)
    }
}

/// Compression processor example
pub struct CompressionProcessor {
    level: u32,
}

impl CompressionProcessor {
    /// Create a new compression processor
    #[must_use]
    pub fn new(level: u32) -> Self {
        Self { level }
    }
}

impl MessageProcessor for CompressionProcessor {
    fn process(&self, message: Message) -> Result<Message> {
        use std::io::Write;

        // Simulate CPU-intensive compression
        let data = message.data();
        let mut encoder =
            flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::new(self.level));

        encoder
            .write_all(data)
            .map_err(|e| NetworkError::other(format!("Compression failed: {}", e)))?;

        let compressed = encoder
            .finish()
            .map_err(|e| NetworkError::other(format!("Compression failed: {}", e)))?;

        Ok(Message::new(compressed))
    }
}

/// Decompression processor
pub struct DecompressionProcessor;

impl MessageProcessor for DecompressionProcessor {
    fn process(&self, message: Message) -> Result<Message> {
        use std::io::Read;

        let data = message.data();
        let mut decoder = flate2::read::GzDecoder::new(&data[..]);
        let mut decompressed = Vec::new();

        decoder
            .read_to_end(&mut decompressed)
            .map_err(|e| NetworkError::other(format!("Decompression failed: {}", e)))?;

        Ok(Message::new(decompressed))
    }
}

/// Encryption processor (example - use real crypto in production)
pub struct EncryptionProcessor {
    key: Vec<u8>,
}

impl EncryptionProcessor {
    /// Create a new encryption processor
    ///
    /// # Security Warning
    ///
    /// This is a simple XOR example. Use real cryptography in production.
    #[must_use]
    pub fn new(key: Vec<u8>) -> Self {
        Self { key }
    }
}

impl MessageProcessor for EncryptionProcessor {
    fn process(&self, message: Message) -> Result<Message> {
        if self.key.is_empty() {
            return Err(NetworkError::other("Encryption key is empty"));
        }

        // Simple XOR encryption (example only)
        let data = message.data();
        let mut encrypted = Vec::with_capacity(data.len());

        for (i, byte) in data.iter().enumerate() {
            let key_byte = self.key[i % self.key.len()];
            encrypted.push(byte ^ key_byte);
        }

        Ok(Message::new(encrypted))
    }
}

/// Message processing pipeline that chains multiple processors
pub struct ProcessorPipeline {
    processors: Vec<Arc<dyn MessageProcessor>>,
}

impl ProcessorPipeline {
    /// Create a new empty pipeline
    #[must_use]
    pub fn new() -> Self {
        Self {
            processors: Vec::new(),
        }
    }

    /// Add a processor to the pipeline
    #[must_use]
    pub fn add<P: MessageProcessor + 'static>(mut self, processor: P) -> Self {
        self.processors.push(Arc::new(processor));
        self
    }

    /// Add an Arc processor to the pipeline
    #[must_use]
    pub fn add_arc(mut self, processor: Arc<dyn MessageProcessor>) -> Self {
        self.processors.push(processor);
        self
    }
}

impl Default for ProcessorPipeline {
    fn default() -> Self {
        Self::new()
    }
}

impl MessageProcessor for ProcessorPipeline {
    fn process(&self, mut message: Message) -> Result<Message> {
        for processor in &self.processors {
            message = processor.process(message)?;
        }
        Ok(message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_identity_processor() {
        let processor = IdentityProcessor;
        let msg = Message::new("test");
        let result = processor.process(msg.clone()).unwrap();
        assert_eq!(msg.data(), result.data());
    }

    #[test]
    fn test_encryption_roundtrip() {
        let key = b"secret_key_12345".to_vec();
        let encrypt = EncryptionProcessor::new(key.clone());
        let decrypt = EncryptionProcessor::new(key); // XOR is symmetric

        let original = Message::new("Hello, World!");
        let encrypted = encrypt.process(original.clone()).unwrap();
        let decrypted = decrypt.process(encrypted).unwrap();

        assert_eq!(original.data(), decrypted.data());
    }

    #[test]
    fn test_processor_pipeline() {
        let key = b"test_key".to_vec();
        let pipeline = ProcessorPipeline::new()
            .add(EncryptionProcessor::new(key.clone()))
            .add(IdentityProcessor);

        let msg = Message::new("test data");
        let result = pipeline.process(msg).unwrap();

        // Should be encrypted
        assert_ne!(result.data().as_ref(), b"test data");
    }
}
