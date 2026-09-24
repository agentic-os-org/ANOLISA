//! Shared SQLite lifecycle primitives for AgentSight stores.

mod connection;
mod lock;
mod measurement;
mod policy;
mod worker;
#[cfg(test)]
mod worker_tests;

use std::path::PathBuf;

pub use connection::{
    CheckpointOutcome, ConnectionMode, ConnectionOptions, checkpoint_truncate, is_retryable_lock,
    open_connection, retry_with_backoff,
};
pub use lock::{MaintenanceLock, MaintenanceLockAcquire};
pub use measurement::{SizeSnapshot, measure_database};
pub use policy::{
    MaintenanceReport, MaintenanceStatus, SizeBasis, SizePolicy, enforce_size_policy,
    retention_cutoff_ns,
};
pub use worker::{
    MaintenanceJob, MaintenanceJobResult, MaintenanceJobState, MaintenanceWorker,
    MaintenanceWorkerState,
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
    /// A maintenance lock file could not be opened or locked.
    #[error("failed to {operation} maintenance lock {path}: {source}")]
    MaintenanceLock {
        /// Operation that failed.
        operation: &'static str,
        /// Lock file involved in the failed operation.
        path: PathBuf,
        /// Underlying filesystem error.
        #[source]
        source: std::io::Error,
    },
    /// A policy contains an invalid value combination.
    #[error("invalid SQLite lifecycle policy: {0}")]
    InvalidPolicy(&'static str),
    /// A maintenance job cannot be scheduled safely.
    #[error("invalid SQLite maintenance job: {0}")]
    InvalidMaintenanceJob(String),
    /// A schema-aware maintenance job failed.
    #[error("SQLite maintenance job failed: {0}")]
    MaintenanceJobFailed(String),
    /// The operating system could not start the maintenance worker thread.
    #[error("failed to start SQLite maintenance worker: {0}")]
    MaintenanceWorkerStart(#[source] std::io::Error),
    /// A panic poisoned the maintenance worker state lock.
    #[error("SQLite maintenance worker state lock is poisoned")]
    MaintenanceWorkerStatePoisoned,
    /// The maintenance worker panicked outside a contained job invocation.
    #[error("SQLite maintenance worker thread panicked")]
    MaintenanceWorkerPanicked,
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
