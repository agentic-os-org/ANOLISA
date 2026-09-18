//! Rotation-aware `JSONL` append log for security events and observability.
//!
//! Migrated from v1 `security_events/writer.py` plus the thin
//! `observability/writer.py` wrapper. The crate deliberately has no `SQLite`
//! dependency so consumers that only need the append log do not pull in
//! `rusqlite` and its bundled C sources.

#![forbid(unsafe_code)]

pub mod error;
pub mod jsonl;
mod observability;
mod security_events;

pub use error::EventLogError;
pub use jsonl::{
    DEFAULT_BACKUP_COUNT, DEFAULT_ERROR_PREFIX, DEFAULT_MAX_BYTES, ErrorHandler, JsonlEventWriter,
    is_backup_suffix,
};
pub use observability::{
    DEFAULT_OBSERVABILITY_BACKUP_COUNT, DEFAULT_OBSERVABILITY_MAX_BYTES, ObservabilityWriter,
};
pub use security_events::SecurityEventWriter;
