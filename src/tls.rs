//! TLS support for secure network communication

#[cfg(feature = "tls")]
pub mod config;

#[cfg(feature = "tls")]
pub use config::{CertificatePins, TlsConfig};

#[cfg(feature = "tls")]
pub use tokio_rustls::TlsAcceptor;

#[cfg(feature = "tls")]
pub use tokio_rustls::TlsConnector;
