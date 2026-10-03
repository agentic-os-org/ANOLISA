use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tempfile::TempDir;

use super::*;
use crate::storage::SqliteTaskStore;

fn database() -> (TempDir, PathBuf) {
    let directory = TempDir::new().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let path = directory.path().join("gateway.db");
    drop(SqliteTaskStore::open(&path).unwrap());
    (directory, path)
}

fn inspect_without_mutation(path: &Path) -> StoreInspection {
    let before = fs::read(path).unwrap();
    let report = inspect_task_store(path).unwrap();
    assert_eq!(fs::read(path).unwrap(), before);
    report
}

fn inspect_failed_store_without_mutation(path: &Path) -> StoreInspection {
    let before = fs::read(path).unwrap();
    assert!(SqliteTaskStore::open(path).is_err());
    assert_eq!(fs::read(path).unwrap(), before);
    inspect_without_mutation(path)
}

#[test]
fn healthy_store_is_inspected_read_only() {
    let (_directory, path) = database();
    let report = inspect_without_mutation(&path);

    assert_eq!(report.outcome, StoreInspectionOutcome::Healthy);
    assert_eq!(
        report.observed_schema_version,
        Some(schema::CURRENT_SCHEMA_VERSION)
    );
    assert_eq!(report.migration_history, StoreCheckStatus::Passed);
    assert_eq!(report.integrity, StoreCheckStatus::Passed);
    assert_eq!(report.foreign_keys, StoreCheckStatus::Passed);
    assert!(!report.read_only_required);
}

#[test]
fn incompatible_migrations_are_redacted_and_read_only() {
    for mutation in [
        "INSERT INTO schema_migrations(version, checksum, applied_at_ms) VALUES (99, 'secret-newer-checksum', 1)",
        "UPDATE schema_migrations SET checksum='secret-changed-checksum' WHERE version=1",
    ] {
        let (_directory, path) = database();
        Connection::open(&path)
            .unwrap()
            .execute_batch(mutation)
            .unwrap();

        let report = inspect_failed_store_without_mutation(&path);
        assert_eq!(report.outcome, StoreInspectionOutcome::Incompatible);
        assert_eq!(report.migration_history, StoreCheckStatus::Failed);
        assert!(report.read_only_required);
        let encoded = serde_json::to_string(&report).unwrap();
        assert!(!encoded.contains("secret"));
        assert!(!encoded.contains("checksum"));
    }
}

#[test]
fn foreign_key_corruption_reports_only_bounded_status() {
    let (_directory, path) = database();
    Connection::open(&path)
        .unwrap()
        .execute_batch(
            "PRAGMA foreign_keys=OFF;
             CREATE TABLE secret_parent(id INTEGER PRIMARY KEY);
             CREATE TABLE secret_customer_rows(
                 id INTEGER PRIMARY KEY,
                 parent_id INTEGER REFERENCES secret_parent(id)
             );
             INSERT INTO secret_customer_rows(id, parent_id) VALUES (7, 9001);",
        )
        .unwrap();

    let report = inspect_failed_store_without_mutation(&path);
    assert_eq!(report.outcome, StoreInspectionOutcome::Corrupt);
    assert_eq!(report.foreign_keys, StoreCheckStatus::Failed);
    let encoded = serde_json::to_string(&report).unwrap();
    assert!(!encoded.contains("secret_customer_rows"));
    assert!(!encoded.contains("9001"));
}

#[test]
fn truncated_database_remains_inspectable_without_raw_bytes() {
    let (_directory, path) = database();
    fs::write(&path, b"private-row-fragment-not-a-database").unwrap();

    let report = inspect_failed_store_without_mutation(&path);
    assert_eq!(report.outcome, StoreInspectionOutcome::Corrupt);
    assert!(report.read_only_required);
    assert!(!serde_json::to_string(&report)
        .unwrap()
        .contains("private-row-fragment"));
}
