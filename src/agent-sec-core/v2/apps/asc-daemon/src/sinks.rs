//! Explicit-path event sink adapter for the daemon composition root.

use std::sync::Arc;

use asc_action_runtime::SecurityEventSink;
use asc_event_sink::ConfiguredSecurityEventSinks;
use asc_security_events::SecurityEvent;

/// Bridges the action runtime's port to configured durable event sinks.
#[derive(Clone)]
pub(crate) struct EventSinkAdapter {
    sinks: Arc<ConfiguredSecurityEventSinks>,
}

impl EventSinkAdapter {
    /// Wraps explicit-path configured sinks.
    pub(crate) fn new(sinks: Arc<ConfiguredSecurityEventSinks>) -> Self {
        Self { sinks }
    }
}

impl SecurityEventSink for EventSinkAdapter {
    fn write(&self, event: &SecurityEvent) {
        self.sinks.log_event(event);
    }
}
