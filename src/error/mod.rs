//! Error types for the network system

use std::io;
use thiserror::Error;

/// Result type for network operations
pub type Result<T> = std::result::Result<T, NetworkError>;

/// Network system errors
#[derive(Debug, Error)]
pub enum NetworkError {
    /// I/O error
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),

    /// Connection error
    #[error("Connection error: {0}")]
    Connection(String),

    /// Timeout error
    #[error("Operation timed out: {0}")]
    Timeout(String),

    /// Serialization error
    #[error("Serialization error: {0}")]
    Serialization(String),

    /// Invalid state error
    #[error("Invalid state: {0}")]
    InvalidState(String),

    /// Address parse error
    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    /// Message too large
    #[error("Message too large: {0} bytes (max: {1} bytes)")]
    MessageTooLarge(usize, usize),

    /// Channel error
    #[error("Channel error: {0}")]
    Channel(String),

    /// TLS error
    #[error("TLS error: {0}")]
    Tls(String),

    /// Other errors
    #[error("Network error: {0}")]
    Other(String),
}

impl NetworkError {
    /// Create a connection error
    pub fn connection<S: Into<String>>(msg: S) -> Self {
        Self::Connection(msg.into())
    }

    /// Create a timeout error
    pub fn timeout<S: Into<String>>(msg: S) -> Self {
        Self::Timeout(msg.into())
    }

    /// Create a serialization error
    pub fn serialization<S: Into<String>>(msg: S) -> Self {
        Self::Serialization(msg.into())
    }

    /// Create an invalid state error
    pub fn invalid_state<S: Into<String>>(msg: S) -> Self {
        Self::InvalidState(msg.into())
    }

    /// Create an invalid address error
    pub fn invalid_address<S: Into<String>>(msg: S) -> Self {
        Self::InvalidAddress(msg.into())
    }

    /// Create a channel error
    pub fn channel<S: Into<String>>(msg: S) -> Self {
        Self::Channel(msg.into())
    }

    /// Create a TLS error
    pub fn tls<S: Into<String>>(msg: S) -> Self {
        Self::Tls(msg.into())
    }

    /// Create an other error
    pub fn other<S: Into<String>>(msg: S) -> Self {
        Self::Other(msg.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_creation() {
        let err = NetworkError::connection("test");
        assert_eq!(err.to_string(), "Connection error: test");

        let err = NetworkError::timeout("operation");
        assert_eq!(err.to_string(), "Operation timed out: operation");

        let err = NetworkError::MessageTooLarge(1000, 512);
        assert_eq!(
            err.to_string(),
            "Message too large: 1000 bytes (max: 512 bytes)"
        );
    }
}
