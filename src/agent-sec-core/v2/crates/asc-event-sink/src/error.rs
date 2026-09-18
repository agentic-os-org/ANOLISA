//! The one error type the assembly layer surfaces.

use asc_event_log::EventLogError;
use asc_persistence_sqlite::observability::ObservabilityWriterError;
use asc_persistence_sqlite::security_events::WriterError;
use asc_security_events::ConfigError;
use asc_sqlite_kernel::KernelError;

/// Failure of building or using a process-wide sink.
///
/// v1 raises whatever the underlying layer raised; the three variants here are
/// exactly the three layers a sink can fail in, flattened so callers do not have
/// to match on two nested per-stream wrappers.
#[derive(Debug, thiserror::Error)]
pub enum SinkError {
    /// A data directory or stream path could not be resolved.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The `JSONL` append log failed.
    #[error(transparent)]
    EventLog(#[from] EventLogError),
    /// The `SQLite` store failed.
    #[error(transparent)]
    Kernel(#[from] KernelError),
}

impl From<WriterError> for SinkError {
    fn from(value: WriterError) -> Self {
        match value {
            WriterError::Config(error) => Self::Config(error),
            WriterError::Kernel(error) => Self::Kernel(error),
        }
    }
}

impl From<ObservabilityWriterError> for SinkError {
    fn from(value: ObservabilityWriterError) -> Self {
        match value {
            ObservabilityWriterError::Config(error) => Self::Config(error),
            ObservabilityWriterError::Kernel(error) => Self::Kernel(error),
        }
    }
}
