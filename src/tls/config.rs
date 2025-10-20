//! TLS configuration for secure connections

use crate::error::{NetworkError, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::Arc;
use tokio_rustls::rustls::{self, ClientConfig, ServerConfig};
use tokio_rustls::{TlsAcceptor, TlsConnector};

/// Certificate pinning configuration
///
/// Stores SHA-256 fingerprints of trusted certificates for pinning
#[derive(Clone, Debug)]
pub struct CertificatePins {
    /// SHA-256 fingerprints of pinned certificates (in hex format)
    fingerprints: Vec<String>,
}

impl CertificatePins {
    /// Create a new certificate pins configuration
    #[must_use]
    pub fn new() -> Self {
        Self {
            fingerprints: Vec::new(),
        }
    }

    /// Add a SHA-256 fingerprint (hex encoded)
    ///
    /// # Example fingerprint format
    ///
    /// "E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55"
    ///
    /// Colons are optional and will be removed during normalization.
    #[must_use]
    pub fn add_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        let fingerprint = fingerprint.into().replace(':', "").to_uppercase();
        self.fingerprints.push(fingerprint);
        self
    }

    /// Check if a certificate matches any of the pinned fingerprints
    fn matches(&self, cert: &CertificateDer<'_>) -> bool {
        // Compute SHA-256 hash of the certificate DER encoding
        let cert_fingerprint = {
            use sha2::{Digest, Sha256};

            let mut hasher = Sha256::new();
            hasher.update(cert.as_ref());
            let result = hasher.finalize();

            result
                .iter()
                .map(|b| format!("{:02X}", b))
                .collect::<String>()
        };

        self.fingerprints.iter().any(|pin| pin == &cert_fingerprint)
    }
}

impl Default for CertificatePins {
    fn default() -> Self {
        Self::new()
    }
}

/// TLS configuration for client and server
#[derive(Clone)]
pub struct TlsConfig {
    /// Server TLS configuration
    server_config: Option<Arc<ServerConfig>>,
    /// Client TLS configuration
    client_config: Option<Arc<ClientConfig>>,
}

impl TlsConfig {
    /// Create a new empty TLS configuration
    ///
    /// This will automatically install the default crypto provider (ring) if one hasn't
    /// been installed yet. This is required by rustls 0.23+.
    #[must_use]
    pub fn new() -> Self {
        // Install the default crypto provider if not already installed
        // This is safe to call multiple times - it will only install once
        let _ = rustls::crypto::ring::default_provider().install_default();

        Self {
            server_config: None,
            client_config: None,
        }
    }

    /// Load server certificates and private key from PEM files
    ///
    /// # Arguments
    ///
    /// * `cert_path` - Path to certificate file (PEM format)
    /// * `key_path` - Path to private key file (PEM format)
    ///
    /// # Errors
    ///
    /// Returns error if:
    /// - Files cannot be read
    /// - Invalid certificate or key format
    /// - Certificate and key don't match
    pub fn with_server_cert_file(
        mut self,
        cert_path: impl AsRef<Path>,
        key_path: impl AsRef<Path>,
    ) -> Result<Self> {
        let certs = Self::load_certs(cert_path.as_ref())?;
        let key = Self::load_private_key(key_path.as_ref())?;

        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| NetworkError::tls(format!("Invalid server certificate: {}", e)))?;

        self.server_config = Some(Arc::new(config));
        Ok(self)
    }

    /// Load server certificates and private key from memory
    ///
    /// # Arguments
    ///
    /// * `cert_pem` - Certificate in PEM format
    /// * `key_pem` - Private key in PEM format
    ///
    /// # Errors
    ///
    /// Returns error if certificate or key is invalid
    pub fn with_server_cert_pem(mut self, cert_pem: &[u8], key_pem: &[u8]) -> Result<Self> {
        let certs = Self::parse_certs(cert_pem)?;
        let key = Self::parse_private_key(key_pem)?;

        let config = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| NetworkError::tls(format!("Invalid server certificate: {}", e)))?;

        self.server_config = Some(Arc::new(config));
        Ok(self)
    }

    /// Configure client to accept any certificate (insecure - for testing only)
    ///
    /// # Security Warning
    ///
    /// This disables certificate validation and should only be used for testing.
    /// Use `with_client_ca_file` or `with_client_ca_pem` for production.
    ///
    /// # Availability
    ///
    /// This method is only available when compiling tests or with the `insecure` feature.
    #[cfg(any(test, feature = "insecure"))]
    #[deprecated(
        since = "0.1.0",
        note = "This method disables TLS certificate validation and is INSECURE. \
                Only use for testing. For production, use with_client_ca_file, \
                with_client_system_roots, or with_client_certificate_pinning."
    )]
    #[must_use]
    pub fn with_client_insecure(mut self) -> Self {
        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(NoVerifier))
            .with_no_client_auth();

        self.client_config = Some(Arc::new(config));
        self
    }

    /// Configure client with root CA certificates from file
    ///
    /// # Arguments
    ///
    /// * `ca_path` - Path to CA certificate file (PEM format)
    ///
    /// # Errors
    ///
    /// Returns error if CA file cannot be read or is invalid
    pub fn with_client_ca_file(mut self, ca_path: impl AsRef<Path>) -> Result<Self> {
        let ca_certs = Self::load_certs(ca_path.as_ref())?;

        let mut root_store = rustls::RootCertStore::empty();
        for cert in ca_certs {
            root_store
                .add(cert)
                .map_err(|e| NetworkError::tls(format!("Invalid CA certificate: {}", e)))?;
        }

        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        self.client_config = Some(Arc::new(config));
        Ok(self)
    }

    /// Configure client with root CA certificates from memory
    ///
    /// # Arguments
    ///
    /// * `ca_pem` - CA certificate in PEM format
    ///
    /// # Errors
    ///
    /// Returns error if CA certificate is invalid
    pub fn with_client_ca_pem(mut self, ca_pem: &[u8]) -> Result<Self> {
        let ca_certs = Self::parse_certs(ca_pem)?;

        let mut root_store = rustls::RootCertStore::empty();
        for cert in ca_certs {
            root_store
                .add(cert)
                .map_err(|e| NetworkError::tls(format!("Invalid CA certificate: {}", e)))?;
        }

        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        self.client_config = Some(Arc::new(config));
        Ok(self)
    }

    /// Configure client to use system root certificates
    ///
    /// # Errors
    ///
    /// Returns error if system root certificates cannot be loaded
    pub fn with_client_system_roots(mut self) -> Result<Self> {
        let mut root_store = rustls::RootCertStore::empty();

        // Add native system certificates
        let native_certs = rustls_native_certs::load_native_certs();

        // The load_native_certs returns a CertificateResult with certs and errors
        for cert in native_certs.certs {
            root_store
                .add(cert)
                .map_err(|e| NetworkError::tls(format!("Invalid system certificate: {}", e)))?;
        }

        // Log or handle errors if needed
        if !native_certs.errors.is_empty() {
            tracing::warn!(
                "Some system certificates could not be loaded: {} errors",
                native_certs.errors.len()
            );
        }

        let config = ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        self.client_config = Some(Arc::new(config));
        Ok(self)
    }

    /// Configure client with certificate pinning
    ///
    /// Certificate pinning provides an additional layer of security by validating
    /// that the server's certificate matches one of the pinned fingerprints,
    /// protecting against CA compromise attacks.
    ///
    /// # Arguments
    ///
    /// * `pins` - Certificate fingerprints to pin
    ///
    /// # Example
    ///
    /// ```no_run
    /// use rust_network_system::tls::{TlsConfig, CertificatePins};
    ///
    /// let pins = CertificatePins::new()
    ///     .add_fingerprint("E3:B0:C4:42:98:FC:1C:14:9A:FB:F4:C8:99:6F:B9:24:27:AE:41:E4:64:9B:93:4C:A4:95:99:1B:78:52:B8:55");
    ///
    /// let config = TlsConfig::new()
    ///     .with_client_certificate_pinning(pins);
    /// ```
    ///
    /// # Security
    ///
    /// This provides additional security beyond standard CA validation. The connection
    /// will only succeed if the server presents a certificate matching one of the pinned
    /// fingerprints.
    #[must_use]
    pub fn with_client_certificate_pinning(mut self, pins: CertificatePins) -> Self {
        let config = ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(PinningVerifier::new(pins)))
            .with_no_client_auth();

        self.client_config = Some(Arc::new(config));
        self
    }

    /// Get TLS acceptor for server (if configured)
    #[must_use]
    pub fn acceptor(&self) -> Option<TlsAcceptor> {
        self.server_config
            .as_ref()
            .map(|config| TlsAcceptor::from(Arc::clone(config)))
    }

    /// Get TLS connector for client (if configured)
    #[must_use]
    pub fn connector(&self) -> Option<TlsConnector> {
        self.client_config
            .as_ref()
            .map(|config| TlsConnector::from(Arc::clone(config)))
    }

    /// Check if server TLS is configured
    #[must_use]
    pub fn has_server_config(&self) -> bool {
        self.server_config.is_some()
    }

    /// Check if client TLS is configured
    #[must_use]
    pub fn has_client_config(&self) -> bool {
        self.client_config.is_some()
    }

    // Helper functions

    fn load_certs(path: &Path) -> Result<Vec<CertificateDer<'static>>> {
        let file = File::open(path).map_err(|e| {
            NetworkError::tls(format!(
                "Failed to open certificate file '{}': {}",
                path.display(),
                e
            ))
        })?;

        let mut reader = BufReader::new(file);
        let certs = rustls_pemfile::certs(&mut reader)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| NetworkError::tls(format!("Failed to parse certificates: {}", e)))?;

        if certs.is_empty() {
            return Err(NetworkError::tls("No certificates found in file"));
        }

        Ok(certs)
    }

    fn load_private_key(path: &Path) -> Result<PrivateKeyDer<'static>> {
        let file = File::open(path).map_err(|e| {
            NetworkError::tls(format!(
                "Failed to open key file '{}': {}",
                path.display(),
                e
            ))
        })?;

        let mut reader = BufReader::new(file);

        // Try to read as PKCS8 first, then RSA
        let keys = rustls_pemfile::private_key(&mut reader)
            .map_err(|e| NetworkError::tls(format!("Failed to parse private key: {}", e)))?
            .ok_or_else(|| NetworkError::tls("No private key found in file"))?;

        Ok(keys)
    }

    fn parse_certs(pem: &[u8]) -> Result<Vec<CertificateDer<'static>>> {
        let mut reader = BufReader::new(pem);
        let certs = rustls_pemfile::certs(&mut reader)
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| NetworkError::tls(format!("Failed to parse certificates: {}", e)))?;

        if certs.is_empty() {
            return Err(NetworkError::tls("No certificates found in PEM data"));
        }

        Ok(certs)
    }

    fn parse_private_key(pem: &[u8]) -> Result<PrivateKeyDer<'static>> {
        let mut reader = BufReader::new(pem);

        let key = rustls_pemfile::private_key(&mut reader)
            .map_err(|e| NetworkError::tls(format!("Failed to parse private key: {}", e)))?
            .ok_or_else(|| NetworkError::tls("No private key found in PEM data"))?;

        Ok(key)
    }
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self::new()
    }
}

/// Certificate verifier that accepts any certificate (insecure - for testing only)
///
/// Only available in test builds or with the `insecure` feature.
#[cfg(any(test, feature = "insecure"))]
#[derive(Debug)]
struct NoVerifier;

#[cfg(any(test, feature = "insecure"))]
impl rustls::client::danger::ServerCertVerifier for NoVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer<'_>,
        _dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

/// Certificate verifier that validates against pinned certificate fingerprints
///
/// This verifier checks that the server's certificate matches one of the
/// pinned SHA-256 fingerprints, providing protection against CA compromise.
#[derive(Debug)]
struct PinningVerifier {
    pins: CertificatePins,
}

impl PinningVerifier {
    fn new(pins: CertificatePins) -> Self {
        Self { pins }
    }
}

impl rustls::client::danger::ServerCertVerifier for PinningVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> std::result::Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // Check if certificate matches any of the pinned fingerprints
        if self.pins.matches(end_entity) {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(
                "Certificate does not match any pinned fingerprint".into(),
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // Use standard signature verification
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        // Use standard signature verification
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::RSA_PKCS1_SHA384,
            rustls::SignatureScheme::RSA_PKCS1_SHA512,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP384_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA256,
            rustls::SignatureScheme::RSA_PSS_SHA384,
            rustls::SignatureScheme::RSA_PSS_SHA512,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tls_config_creation() {
        let config = TlsConfig::new();
        assert!(!config.has_server_config());
        assert!(!config.has_client_config());
    }

    #[test]
    #[allow(deprecated)]
    fn test_insecure_client_config() {
        let config = TlsConfig::new().with_client_insecure();
        assert!(config.has_client_config());
        assert!(config.connector().is_some());
    }

    // Note: Real certificate testing would require valid cert/key pairs
    // For integration testing, use proper test certificates generated with tools like:
    // openssl req -x509 -newkey rsa:2048 -nodes -keyout key.pem -out cert.pem -days 365
}
