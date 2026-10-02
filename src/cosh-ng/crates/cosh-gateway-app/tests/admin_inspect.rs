//! Local CLI coverage for read-only corrupt-store inspection.

use std::fs;
use std::process::Command;

use cosh_gateway::storage::SqliteTaskStore;
use rusqlite::Connection;

#[test]
fn admin_inspect_reports_incompatible_store_without_mutation_or_raw_rows() {
    let directory = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let database = directory.path().join("gateway.db");
    drop(SqliteTaskStore::open(&database).unwrap());
    Connection::open(&database)
        .unwrap()
        .execute(
            "UPDATE schema_migrations SET checksum='private-row-secret' WHERE version=1",
            [],
        )
        .unwrap();
    let before = fs::read(&database).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_cosh-gateway"))
        .args([
            "admin",
            "--output",
            "jsonl",
            "inspect",
            "--database",
            database.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(14));
    assert_eq!(fs::read(&database).unwrap(), before);
    let stdout = String::from_utf8(output.stdout).unwrap();
    let report: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(report["event"], "store_inspection");
    assert_eq!(report["outcome"], "incompatible");
    assert_eq!(report["migration_history"], "failed");
    assert_eq!(report["read_only_required"], true);
    assert!(!stdout.contains("private-row-secret"));
}
