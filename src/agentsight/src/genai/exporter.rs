//! GenAI Exporter Trait
//!
//! Defines a pluggable interface for exporting GenAI semantic events.
//! Any component that wants to consume GenAI events (SLS, Kafka, HTTP, etc.)
//! just needs to implement the `GenAIExporter` trait.

use super::semantic::GenAISemanticEvent;

/// Trait for exporting GenAI semantic events to external systems.
///
/// Implementors handle the actual transport (file, SLS, Kafka, HTTP, etc.).
/// The `export` method should be non-blocking or internally buffered
/// to avoid slowing down the main pipeline.
pub trait GenAIExporter: Send {
    /// Human-readable name for this exporter (used in logs)
    fn name(&self) -> &str;

    /// Whether this exporter currently accepts events.
    ///
    /// Inactive sinks remain registered and are checked for each delivery.
    /// Exporters without an availability gate conservatively remain active.
    fn is_active(&self) -> bool {
        true
    }

    /// Export a batch of GenAI semantic events.
    ///
    /// This method should not block the caller for long.
    /// Implementations should buffer internally or use async background threads.
    fn export(&self, events: &[GenAISemanticEvent]);
}
