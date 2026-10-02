//! Stream configuration for observability persistence.
//!
//! Migrated from v1 `observability/config.py`, which reuses the
//! security-events data-directory resolution rather than defining its own.

use std::path::{Path, PathBuf};

use asc_security_events::ConfigError;
use asc_security_events::config::{
    get_stream_db_path, get_stream_log_path, stream_db_path_in, stream_log_path_in,
};

/// Stream name for observability records.
pub const OBSERVABILITY_STREAM: &str = "observability";

/// Prefix used on observability diagnostic lines.
pub const OBSERVABILITY_LOG_PREFIX: &str = "[observability]";

/// Retention window applied by the observability `SQLite` writer, in days.
pub const DEFAULT_OBSERVABILITY_RETENTION_DAYS: u32 = 7;

/// Returns the `JSONL` path for the observability stream.
///
/// # Errors
///
/// Propagates the data-directory resolution failure from the security-events
/// config layer.
pub fn get_observability_log_path() -> Result<PathBuf, ConfigError> {
    get_stream_log_path(OBSERVABILITY_STREAM)
}

/// Returns the `SQLite` path for the observability stream.
///
/// # Errors
///
/// Propagates the data-directory resolution failure from the security-events
/// config layer.
pub fn get_observability_db_path() -> Result<PathBuf, ConfigError> {
    get_stream_db_path(OBSERVABILITY_STREAM)
}

/// Returns the observability `JSONL` path under an explicit data directory.
///
/// Tests use this form so they never have to mutate process environment.
///
/// # Errors
///
/// Returns an error only if the stream name fails validation, which cannot
/// happen for the built-in stream.
pub fn observability_log_path_in(data_dir: &Path) -> Result<PathBuf, ConfigError> {
    stream_log_path_in(data_dir, OBSERVABILITY_STREAM)
}

/// Returns the observability `SQLite` path under an explicit data directory.
///
/// # Errors
///
/// Returns an error only if the stream name fails validation, which cannot
/// happen for the built-in stream.
pub fn observability_db_path_in(data_dir: &Path) -> Result<PathBuf, ConfigError> {
    stream_db_path_in(data_dir, OBSERVABILITY_STREAM)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constants_match_v1() {
        assert_eq!(OBSERVABILITY_STREAM, "observability");
        assert_eq!(OBSERVABILITY_LOG_PREFIX, "[observability]");
        assert_eq!(DEFAULT_OBSERVABILITY_RETENTION_DAYS, 7);
    }

    #[test]
    fn injected_paths_use_the_observability_stream() {
        let dir = Path::new("/tmp/asc-test-data");
        assert_eq!(
            observability_log_path_in(dir).expect("valid stream"),
            dir.join("observability.jsonl")
        );
        assert_eq!(
            observability_db_path_in(dir).expect("valid stream"),
            dir.join("observability.db")
        );
    }
}
