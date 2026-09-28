//! Single-record ingestion with request-scoped `OTel` attribution.
use std::sync::Arc;

use asc_observability::{ObservabilityError, ObservabilityRecord};
use serde_json::{Map, Value, json};

/// Durable ingestion port. Success means both configured destinations accepted the record.
pub trait ObservabilitySink: Send + Sync {
    /// Writes JSONL first, then `SQLite`; an error may follow a successful JSONL append.
    ///
    /// # Errors
    /// Returns a safe failure without exposing storage paths or record contents.
    fn write(&self, record: &ObservabilityRecord) -> Result<(), ObservabilityWriteError>;
}

/// Safe ingestion failures for protocol projection.
#[derive(Debug, thiserror::Error)]
pub enum ObservabilityWriteError {
    /// Invalid domain record or absent required baggage.
    #[error("invalid observability record")]
    Invalid(#[from] ObservabilityError),
    /// One or both writes failed; the operation is not atomic or automatically retried.
    #[error("failed to write observability record")]
    Storage,
}

/// Application operation composed over an explicitly configured persistence port.
pub struct ObservabilityService {
    sink: Arc<dyn ObservabilitySink>,
}

impl ObservabilityService {
    /// Installs the process-owned persistence adapter.
    pub fn new(sink: Arc<dyn ObservabilitySink>) -> Self {
        Self { sink }
    }

    /// Validates and projects current `OTel` baggage into a persisted record.
    ///
    /// # Errors
    /// Returns validation or storage failure; invalid records never reach the sink.
    pub fn record(
        &self,
        hook: &str,
        observed_at: &str,
        metrics: &Map<String, Value>,
    ) -> Result<(), ObservabilityWriteError> {
        // The existing parser requires session/run and, for tool hooks, tool_call_id.
        // It also drops metadata fields not modeled by the selected hook.
        let record = ObservabilityRecord::from_json_value(&json!({
            "hook": hook,
            "observedAt": observed_at,
            "metadata": asc_observability::snapshot().agent,
            "metrics": metrics,
        }))?;
        self.sink.write(&record)
    }
}
