//! Core networking components

pub mod client;
pub mod message;
pub mod message_queue;
pub mod server;

pub use client::{ClientConfig, ClientState, TcpClient};
pub use message::{Message, MAX_MESSAGE_SIZE};
pub use message_queue::{MessageQueue, MessageQueueConfig, MessageQueueStats};
pub use server::{ServerConfig, TcpServer};
