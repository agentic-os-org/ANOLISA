//! Storage module - unified persistence layer
//!
//! This module provides storage abstraction with multiple backend support:
//! - SQLite: Local file-based storage (current implementation)
//! - SLS: Alibaba Cloud Log Service (planned)
//!
//! Use `Storage` for a unified interface that combines all storage types.

use std::path::Path;

pub mod sqlite;
mod unified;

// Re-export from sqlite module
pub use sqlite::{
    // Audit storage
    AuditStore,
    // HTTP storage
    HttpStore,
    SqliteStore,
    TimePeriod,
    TokenBreakdown,
    TokenComparison,
    TokenConsumptionFilter,
    TokenConsumptionQueryResult,
    TokenConsumptionRecord,
    // Token consumption storage
    TokenConsumptionStore,
    TokenQuery,
    TokenQueryResult,
    // Token storage
    TokenStore,
    Trend,
    // Connection utilities
    create_connection,
    default_base_path,
    format_tokens,
    format_tokens_with_commas,
};

// Re-export unified storage
pub use unified::{SqliteConfig, Storage, StorageBackend};

/// Check if a custom data file path exists
///
/// Returns an error if the path does not exist or is not a file.
/// This is used by CLI subcommands when --data-file is specified
/// to fail early with a clear error message instead of creating
/// an empty database and returning misleading zero results.
pub fn check_data_file(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Err(format!("Database not found: {}", path.display()));
    }
    if !path.is_file() {
        return Err(format!("Path is not a file: {}", path.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn test_check_data_file_nonexistent() {
        let path = Path::new("/tmp/agentsight_test_nonexistent_12345.db");
        let result = check_data_file(path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Database not found"));
    }

    #[test]
    fn test_check_data_file_exists() {
        let path = std::env::temp_dir().join("agentsight_test_exists.db");
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(b"test").unwrap();
        drop(f);

        let result = check_data_file(&path);
        assert!(result.is_ok());

        fs::remove_file(&path).ok();
    }

    #[test]
    fn test_check_data_file_directory() {
        let path = std::env::temp_dir();
        let result = check_data_file(&path);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("not a file"));
    }
}
