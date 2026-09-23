//! Shared SQLite lifecycle primitives for AgentSight stores.

mod connection;
mod measurement;
mod policy;

use std::path::PathBuf;

pub use connection::{
    CheckpointOutcome, ConnectionMode, ConnectionOptions, checkpoint_truncate, is_retryable_lock,
    open_connection, retry_with_backoff,
};
pub use measurement::{SizeSnapshot, measure_database};
pub use policy::{
    MaintenanceReport, MaintenanceStatus, SizeBasis, SizePolicy, enforce_size_policy,
    retention_cutoff_ns,
};

/// Errors produced by SQLite lifecycle operations.
#[derive(Debug, thiserror::Error)]
pub enum LifecycleError {
    /// The requested database does not exist.
    #[error("SQLite database does not exist: {0}")]
    DatabaseMissing(PathBuf),
    /// A filesystem operation failed.
    #[error("failed to access {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// SQLite rejected an operation.
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    /// A policy contains an invalid value combination.
    #[error("invalid SQLite lifecycle policy: {0}")]
    InvalidPolicy(&'static str),
    /// Retention duration cannot be represented in nanoseconds.
    #[error("retention duration overflows nanoseconds")]
    RetentionOverflow,
    /// All configured retries encountered SQLite lock contention.
    #[error("SQLite operation remained locked after {attempts} attempts: {source}")]
    RetryExhausted {
        /// Total number of attempted executions.
        attempts: usize,
        /// Last lock error returned by SQLite.
        #[source]
        source: Box<LifecycleError>,
    },
}
