//! Integration with other systems
//!
//! This module provides adapters to integrate the network system with
//! logging, monitoring, and thread pool systems.

#[cfg(feature = "integration")]
pub mod logger;

#[cfg(feature = "integration")]
pub mod monitor;

#[cfg(feature = "integration")]
pub mod thread_pool;

#[cfg(feature = "integration")]
pub use logger::LoggingSessionHandler;

#[cfg(feature = "integration")]
pub use monitor::MonitoredServer;

#[cfg(feature = "integration")]
pub use thread_pool::{
    CompressionProcessor, DecompressionProcessor, EncryptionProcessor, IdentityProcessor,
    MessageProcessor, ProcessorPipeline, ThreadPoolSessionHandler,
};
