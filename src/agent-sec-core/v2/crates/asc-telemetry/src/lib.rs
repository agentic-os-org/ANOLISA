//! Privacy-bounded telemetry projection shared by all scan capabilities.
#![forbid(unsafe_code)]

pub mod config;
mod record;
pub use record::{ScanTelemetryInput, TelemetryRecord};

/// Outcome of a telemetry write, separated from the capability outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TelemetryStatus {
    /// The complete record was appended (not an fsync durability promise).
    Written,
    /// Disabled, absent target, or a contended nonblocking lock.
    Skipped,
    /// Serialization or I/O failed.
    Failed,
}
