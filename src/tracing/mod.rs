//! Distributed tracing support for network operations

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Global trace ID counter
static TRACE_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate a new trace ID
pub fn generate_trace_id() -> String {
    let id = TRACE_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("trace-{:016x}", id)
}

/// Generate a new span ID
pub fn generate_span_id() -> String {
    use std::time::SystemTime;
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_else(|e| {
            tracing::warn!("System time before Unix epoch: {} - using span counter", e);
            TRACE_COUNTER.fetch_add(1, Ordering::Relaxed)
        });
    format!("span-{:016x}", now)
}

/// Trace context for a network operation
#[derive(Debug, Clone)]
pub struct TraceContext {
    /// Trace ID for request correlation across services
    pub trace_id: String,

    /// Current span ID
    pub span_id: String,

    /// Parent span ID (if any)
    pub parent_span_id: Option<String>,

    /// Service name
    pub service_name: String,

    /// Operation name
    pub operation: String,
}

impl TraceContext {
    /// Create a new trace context (root span)
    pub fn new(service_name: impl Into<String>, operation: impl Into<String>) -> Self {
        Self {
            trace_id: generate_trace_id(),
            span_id: generate_span_id(),
            parent_span_id: None,
            service_name: service_name.into(),
            operation: operation.into(),
        }
    }

    /// Create a child span
    pub fn child_span(&self, operation: impl Into<String>) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: generate_span_id(),
            parent_span_id: Some(self.span_id.clone()),
            service_name: self.service_name.clone(),
            operation: operation.into(),
        }
    }

    /// Create from existing trace and parent span IDs
    pub fn from_parent(
        trace_id: String,
        parent_span_id: String,
        service_name: impl Into<String>,
        operation: impl Into<String>,
    ) -> Self {
        Self {
            trace_id,
            span_id: generate_span_id(),
            parent_span_id: Some(parent_span_id),
            service_name: service_name.into(),
            operation: operation.into(),
        }
    }
}

/// Span for measuring operation duration
pub struct Span {
    context: TraceContext,
    start_time: std::time::Instant,
    attributes: std::collections::HashMap<String, String>,
}

impl Span {
    /// Create a new span
    pub fn new(context: TraceContext) -> Self {
        Self {
            context,
            start_time: std::time::Instant::now(),
            attributes: std::collections::HashMap::new(),
        }
    }

    /// Add an attribute to the span
    pub fn set_attribute(&mut self, key: impl Into<String>, value: impl Into<String>) {
        self.attributes.insert(key.into(), value.into());
    }

    /// Get the trace context
    pub fn context(&self) -> &TraceContext {
        &self.context
    }

    /// Get elapsed time in milliseconds
    pub fn elapsed_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }

    /// End the span and return a summary
    pub fn end(self) -> SpanSummary {
        // Calculate duration before moving any fields to avoid partial move
        let duration_ms = self.elapsed_ms();
        SpanSummary {
            trace_id: self.context.trace_id,
            span_id: self.context.span_id,
            parent_span_id: self.context.parent_span_id,
            service_name: self.context.service_name,
            operation: self.context.operation,
            duration_ms,
            attributes: self.attributes,
        }
    }
}

/// Summary of a completed span
#[derive(Debug, Clone)]
pub struct SpanSummary {
    pub trace_id: String,
    pub span_id: String,
    pub parent_span_id: Option<String>,
    pub service_name: String,
    pub operation: String,
    pub duration_ms: u64,
    pub attributes: std::collections::HashMap<String, String>,
}

/// Span collector for aggregating trace data
pub struct SpanCollector {
    spans: Arc<parking_lot::RwLock<Vec<SpanSummary>>>,
}

impl SpanCollector {
    /// Create a new span collector
    pub fn new() -> Self {
        Self {
            spans: Arc::new(parking_lot::RwLock::new(Vec::new())),
        }
    }

    /// Record a completed span
    pub fn record(&self, span: SpanSummary) {
        self.spans.write().push(span);
    }

    /// Get all recorded spans
    pub fn get_spans(&self) -> Vec<SpanSummary> {
        self.spans.read().clone()
    }

    /// Clear all spans
    pub fn clear(&self) {
        self.spans.write().clear();
    }

    /// Export spans as JSON
    pub fn export_json(&self) -> Result<String, serde_json::Error> {
        #[derive(serde::Serialize)]
        struct SpanExport {
            trace_id: String,
            span_id: String,
            parent_span_id: Option<String>,
            service: String,
            operation: String,
            duration_ms: u64,
            attributes: std::collections::HashMap<String, String>,
        }

        let spans: Vec<SpanExport> = self
            .spans
            .read()
            .iter()
            .map(|s| SpanExport {
                trace_id: s.trace_id.clone(),
                span_id: s.span_id.clone(),
                parent_span_id: s.parent_span_id.clone(),
                service: s.service_name.clone(),
                operation: s.operation.clone(),
                duration_ms: s.duration_ms,
                attributes: s.attributes.clone(),
            })
            .collect();

        serde_json::to_string_pretty(&spans)
    }
}

impl Default for SpanCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_id_generation() {
        let id1 = generate_trace_id();
        let id2 = generate_trace_id();
        assert_ne!(id1, id2);
        assert!(id1.starts_with("trace-"));
    }

    #[test]
    fn test_trace_context() {
        let ctx = TraceContext::new("my-service", "operation");
        assert_eq!(ctx.service_name, "my-service");
        assert_eq!(ctx.operation, "operation");
        assert!(ctx.parent_span_id.is_none());
    }

    #[test]
    fn test_child_span() {
        let parent = TraceContext::new("service", "parent-op");
        let child = parent.child_span("child-op");

        assert_eq!(child.trace_id, parent.trace_id);
        assert_ne!(child.span_id, parent.span_id);
        assert_eq!(child.parent_span_id, Some(parent.span_id));
    }

    #[test]
    fn test_span_duration() {
        let ctx = TraceContext::new("service", "test");
        let span = Span::new(ctx);

        std::thread::sleep(std::time::Duration::from_millis(10));

        let summary = span.end();
        assert!(summary.duration_ms >= 10);
    }

    #[test]
    fn test_span_collector() {
        let collector = SpanCollector::new();

        let ctx = TraceContext::new("service", "op1");
        let mut span = Span::new(ctx);
        span.set_attribute("key", "value");

        let summary = span.end();
        collector.record(summary);

        let spans = collector.get_spans();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].operation, "op1");
    }
}
