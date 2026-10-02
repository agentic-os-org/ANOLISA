//! Read-only, redacted health inspection for unavailable Task stores.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Serialize;

use super::{schema, sqlite, StoreError};

/// Coarse inspection outcome safe for local operator output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreInspectionOutcome {
    /// Every supported read-only check passed.
    Healthy,
    /// Migration history requires a newer or different binary.
    Incompatible,
    /// At least one bounded consistency check failed.
    Corrupt,
}

/// Bounded result for one diagnostic check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StoreCheckStatus {
    /// The check completed without detecting a problem.
    Passed,
    /// The check detected a problem without exposing stored values.
    Failed,
    /// Corruption prevented the check from completing.
    Unavailable,
    /// The checked object does not exist in this schema generation.
    NotPresent,
}

/// Aggregate state of legacy Runtime-start recovery markers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LegacyMarkerInspection {
    /// Whether the marker table could be inspected.
    pub status: StoreCheckStatus,
    /// Markers still awaiting startup settlement.
    pub pending: u64,
    /// Markers already settled by a prior startup.
    pub settled: u64,
    /// Rows carrying an unrecognized state, without exposing row contents.
    pub invalid: u64,
}

/// Redacted, read-only Task-store inspection report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoreInspection {
    /// Overall fail-closed classification.
    pub outcome: StoreInspectionOutcome,
    /// Highest schema version supported by this binary.
    pub supported_schema_version: u32,
    /// Highest recorded schema version when safely readable.
    pub observed_schema_version: Option<u32>,
    /// Checksummed migration-history status.
    pub migration_history: StoreCheckStatus,
    /// SQLite bounded quick-check status.
    pub integrity: StoreCheckStatus,
    /// Foreign-key consistency status.
    pub foreign_keys: StoreCheckStatus,
    /// Legacy marker aggregate without Task or Run identifiers.
    pub legacy_runtime_markers: LegacyMarkerInspection,
    /// Corrupt or incompatible stores require offline operator handling.
    pub read_only_required: bool,
}

/// Inspects an existing private Task store without migration or repair.
///
/// # Errors
///
/// Rejects unsafe paths or files that cannot be opened read-only. Database
/// corruption is returned as a redacted report rather than as raw row data.
pub fn inspect_task_store(path: impl AsRef<Path>) -> Result<StoreInspection, StoreError> {
    let path = path.as_ref();
    sqlite::validate_existing_private_file_path(path)?;
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    sqlite::configure_read_only(&connection)?;

    let observed_schema_version = observed_schema_version(&connection).ok().flatten();
    let (migration_history, incompatible) = migration_status(&connection);
    let integrity = quick_check(&connection);
    let foreign_keys = foreign_key_check(&connection);
    let legacy_runtime_markers = legacy_markers(&connection);
    let corrupt = matches!(
        migration_history,
        StoreCheckStatus::Failed | StoreCheckStatus::Unavailable
    ) || integrity != StoreCheckStatus::Passed
        || foreign_keys != StoreCheckStatus::Passed
        || matches!(
            legacy_runtime_markers.status,
            StoreCheckStatus::Failed | StoreCheckStatus::Unavailable
        );
    let outcome = if incompatible {
        StoreInspectionOutcome::Incompatible
    } else if corrupt {
        StoreInspectionOutcome::Corrupt
    } else {
        StoreInspectionOutcome::Healthy
    };
    Ok(StoreInspection {
        outcome,
        supported_schema_version: schema::CURRENT_SCHEMA_VERSION,
        observed_schema_version,
        migration_history,
        integrity,
        foreign_keys,
        legacy_runtime_markers,
        read_only_required: outcome != StoreInspectionOutcome::Healthy,
    })
}

fn observed_schema_version(connection: &Connection) -> rusqlite::Result<Option<u32>> {
    let exists = table_exists(connection, "schema_migrations")?;
    if !exists {
        return Ok(None);
    }
    connection
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })
        .optional()
        .map(Option::flatten)
}

fn migration_status(connection: &Connection) -> (StoreCheckStatus, bool) {
    match schema::preflight_existing(connection) {
        Ok(()) => (StoreCheckStatus::Passed, false),
        Err(StoreError::NewerSchema { .. } | StoreError::MigrationChecksum { .. }) => {
            (StoreCheckStatus::Failed, true)
        }
        Err(StoreError::Corrupt { ref message })
            if message.contains("quick_check") || message.contains("foreign_key_check") =>
        {
            (StoreCheckStatus::Passed, false)
        }
        Err(StoreError::Corrupt { .. }) => (StoreCheckStatus::Failed, false),
        Err(_) => (StoreCheckStatus::Unavailable, false),
    }
}

fn quick_check(connection: &Connection) -> StoreCheckStatus {
    match connection.query_row("PRAGMA quick_check(1)", [], |row| row.get::<_, String>(0)) {
        Ok(result) if result == "ok" => StoreCheckStatus::Passed,
        Ok(_) => StoreCheckStatus::Failed,
        Err(_) => StoreCheckStatus::Unavailable,
    }
}

fn foreign_key_check(connection: &Connection) -> StoreCheckStatus {
    match connection
        .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
        .optional()
    {
        Ok(None) => StoreCheckStatus::Passed,
        Ok(Some(())) => StoreCheckStatus::Failed,
        Err(_) => StoreCheckStatus::Unavailable,
    }
}

fn legacy_markers(connection: &Connection) -> LegacyMarkerInspection {
    let exists = match table_exists(connection, "legacy_runtime_start_recoveries") {
        Ok(exists) => exists,
        Err(_) => return unavailable_legacy_markers(),
    };
    if !exists {
        return LegacyMarkerInspection {
            status: StoreCheckStatus::NotPresent,
            pending: 0,
            settled: 0,
            invalid: 0,
        };
    }
    let counts = connection.query_row(
        "SELECT
             COALESCE(SUM(CASE WHEN state = 'pending' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN state = 'settled' THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN state NOT IN ('pending', 'settled') THEN 1 ELSE 0 END), 0)
         FROM legacy_runtime_start_recoveries",
        [],
        |row| {
            Ok((
                row.get::<_, u64>(0)?,
                row.get::<_, u64>(1)?,
                row.get::<_, u64>(2)?,
            ))
        },
    );
    match counts {
        Ok((pending, settled, invalid)) => LegacyMarkerInspection {
            status: if invalid == 0 {
                StoreCheckStatus::Passed
            } else {
                StoreCheckStatus::Failed
            },
            pending,
            settled,
            invalid,
        },
        Err(_) => unavailable_legacy_markers(),
    }
}

fn table_exists(connection: &Connection, table: &str) -> rusqlite::Result<bool> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [table],
        |row| row.get(0),
    )
}

fn unavailable_legacy_markers() -> LegacyMarkerInspection {
    LegacyMarkerInspection {
        status: StoreCheckStatus::Unavailable,
        pending: 0,
        settled: 0,
        invalid: 0,
    }
}

#[cfg(test)]
#[path = "inspect/tests.rs"]
mod tests;
