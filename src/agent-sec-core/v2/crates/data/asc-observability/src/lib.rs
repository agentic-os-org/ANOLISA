//! Observability record contracts and stream configuration.
//!
//! This crate is the contract layer for the observability stream: the record
//! envelope, the per-hook metric allowlist, the stream paths and the `SQLite`
//! schema version. Writing, reading and the process-wide sinks live in sibling
//! crates.
//!
//! Migrated from v1 `agent_sec_cli/observability/{schema,config,metrics,
//! models}.py` plus the summary value types from `repositories.py`.

#![forbid(unsafe_code)]

pub mod config;
pub mod correlation;
pub mod error;
pub mod hook;
pub mod metrics;
mod record;
mod schema_version;
mod summary;

pub use config::{
    DEFAULT_OBSERVABILITY_RETENTION_DAYS, OBSERVABILITY_LOG_PREFIX, OBSERVABILITY_STREAM,
    get_observability_db_path, get_observability_log_path,
};
pub use error::ObservabilityError;
pub use hook::{MetadataShape, OBSERVABILITY_HOOKS, ObservabilityHook};
pub use metrics::{allowed_metrics_for_hook, hook_metric_allowlist};
pub use record::{HookMetrics, ObservabilityMetadata, ObservabilityRecord, format_observed_at};
pub use schema_version::OBSERVABILITY_SQLITE_SCHEMA_VERSION;
pub use summary::{RunSummary, SessionSummary, USER_INPUT_PREVIEW_LIMIT};
