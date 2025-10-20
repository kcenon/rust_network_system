//! Monitoring integration

use crate::core::{ServerConfig, TcpServer};
use crate::error::Result;
use crate::session::{Session, SessionHandler};
use crate::Message;
use async_trait::async_trait;
use rust_monitoring_system::{Counter, Gauge, Monitor};
use std::collections::HashMap;
use std::sync::Arc;

/// Monitored TCP server with metrics
pub struct MonitoredServer {
    server: TcpServer,
    monitor: Arc<Monitor>,

    // Metrics
    connections_total: Counter,
    active_connections: Gauge,
    messages_received_total: Counter,
    messages_sent_total: Counter,
    bytes_received_total: Counter,
    bytes_sent_total: Counter,
}

impl MonitoredServer {
    /// Create a new monitored server
    #[must_use]
    pub fn new(config: ServerConfig, monitor: Arc<Monitor>) -> Self {
        let server = TcpServer::with_config(config);

        // Create metrics
        let connections_total = monitor.counter("network_connections_total", HashMap::new());
        let active_connections = monitor.gauge("network_active_connections", HashMap::new());
        let messages_received_total =
            monitor.counter("network_messages_received_total", HashMap::new());
        let messages_sent_total = monitor.counter("network_messages_sent_total", HashMap::new());
        let bytes_received_total = monitor.counter("network_bytes_received_total", HashMap::new());
        let bytes_sent_total = monitor.counter("network_bytes_sent_total", HashMap::new());

        Self {
            server,
            monitor,
            connections_total,
            active_connections,
            messages_received_total,
            messages_sent_total,
            bytes_received_total,
            bytes_sent_total,
        }
    }

    /// Start the monitored server
    pub async fn start<H>(&mut self, handler: Arc<H>) -> Result<()>
    where
        H: SessionHandler + Send + Sync + 'static,
    {
        // Wrap handler with monitoring
        let monitored_handler = Arc::new(MonitoringSessionHandler {
            inner: handler,
            connections_total: self.connections_total.clone(),
            active_connections: self.active_connections.clone(),
            messages_received_total: self.messages_received_total.clone(),
            bytes_received_total: self.bytes_received_total.clone(),
        });

        self.server.start(monitored_handler).await
    }

    /// Stop the server
    pub async fn stop(&mut self) -> Result<()> {
        self.server.stop().await
    }

    /// Check if server is running
    #[must_use]
    pub fn is_running(&self) -> bool {
        self.server.is_running()
    }

    /// Get active connection count
    #[must_use]
    pub fn connection_count(&self) -> u64 {
        self.server.connection_count()
    }

    /// Get the underlying monitor
    #[must_use]
    pub fn monitor(&self) -> &Arc<Monitor> {
        &self.monitor
    }

    /// Record a sent message
    pub fn record_message_sent(&self, size: usize) {
        self.messages_sent_total.inc();
        self.bytes_sent_total.inc_by(size as u64);
    }
}

/// Session handler that records metrics
struct MonitoringSessionHandler<H: SessionHandler> {
    inner: Arc<H>,
    connections_total: Counter,
    active_connections: Gauge,
    messages_received_total: Counter,
    bytes_received_total: Counter,
}

#[async_trait]
impl<H: SessionHandler + Send + Sync> SessionHandler for MonitoringSessionHandler<H> {
    async fn on_connected(&self, session: &Session) {
        self.connections_total.inc();
        self.active_connections.inc();
        self.inner.on_connected(session).await;
    }

    async fn on_message(&self, session: &Session, message: Message) {
        self.messages_received_total.inc();
        self.bytes_received_total.inc_by(message.size() as u64);
        self.inner.on_message(session, message).await;
    }

    async fn on_disconnected(&self, session: &Session) {
        self.active_connections.dec();
        self.inner.on_disconnected(session).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_monitored_server_creation() {
        let config = ServerConfig::new("127.0.0.1:8080");
        let monitor = Arc::new(Monitor::new());
        let server = MonitoredServer::new(config, monitor);

        assert!(!server.is_running());
        assert_eq!(server.connection_count(), 0);
    }
}
