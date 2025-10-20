//! Logger integration

use crate::session::{Session, SessionHandler};
use crate::Message;
use async_trait::async_trait;
use rust_logger_system::Logger;
use std::sync::Arc;

/// Session handler that logs all events
pub struct LoggingSessionHandler {
    logger: Arc<Logger>,
    inner: Option<Arc<dyn SessionHandler + Send + Sync>>,
}

impl LoggingSessionHandler {
    /// Create a new logging session handler
    #[must_use]
    pub fn new(logger: Arc<Logger>) -> Self {
        Self {
            logger,
            inner: None,
        }
    }

    /// Wrap another session handler with logging
    #[must_use]
    pub fn wrap<H>(logger: Arc<Logger>, inner: Arc<H>) -> Self
    where
        H: SessionHandler + Send + Sync + 'static,
    {
        Self {
            logger,
            inner: Some(inner),
        }
    }
}

#[async_trait]
impl SessionHandler for LoggingSessionHandler {
    async fn on_connected(&self, session: &Session) {
        self.logger.info(format!(
            "Session {} connected from {}",
            session.id(),
            session.peer_addr()
        ));

        if let Some(ref inner) = self.inner {
            inner.on_connected(session).await;
        }
    }

    async fn on_message(&self, session: &Session, message: Message) {
        self.logger.debug(format!(
            "Session {} received message: {}",
            session.id(),
            message
        ));

        if let Some(ref inner) = self.inner {
            inner.on_message(session, message).await;
        }
    }

    async fn on_disconnected(&self, session: &Session) {
        self.logger.info(format!(
            "Session {} disconnected (uptime: {:?})",
            session.id(),
            session.uptime()
        ));

        if let Some(ref inner) = self.inner {
            inner.on_disconnected(session).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_logging_handler_creation() {
        let logger = Arc::new(Logger::new());
        let handler = LoggingSessionHandler::new(logger);
        assert!(handler.inner.is_none());
    }
}
