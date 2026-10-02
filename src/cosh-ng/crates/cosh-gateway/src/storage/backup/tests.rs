use std::collections::BTreeMap;

use cosh_gateway_contracts::common::{
    BoundedName, BoundedOpaque, ContractHeader, ContractSchema, Correlation, Digest, TargetRef,
};
use cosh_gateway_contracts::ids::{ActorId, MessageId, TaskId};
use cosh_gateway_contracts::task::{TaskEvent, TaskEventEnvelope};
use rusqlite::types::ValueRef;

use super::*;

fn source_store(root: &Path) -> (SqliteTaskStore, InstallationId, PathBuf) {
    let source_path = root.join("source/state.db");
    let installation_id = InstallationId::new();
    let mut store = SqliteTaskStore::open(&source_path).unwrap();
    assert_eq!(
        store.bind_installation_id(Some(&installation_id)).unwrap(),
        installation_id
    );
    store
        .connection()
        .execute_batch("PRAGMA wal_autocheckpoint = 0;")
        .unwrap();
    store
        .connection()
        .execute(
            "INSERT INTO ledger_receipts(
                 actor_id, idempotency_key, command_digest, operation,
                 result_json, committed_at_ms
             ) VALUES ('actor', 'backup-marker', ?1, 'test', '{\"ok\":true}', 100)",
            ["a".repeat(64)],
        )
        .unwrap();
    (store, installation_id, source_path)
}

fn copy_private(source: &Path, destination: &Path) {
    fs::copy(source, destination).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        fs::set_permissions(destination, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(
            fs::symlink_metadata(destination).unwrap().mode() & 0o777,
            0o600
        );
    }
}

fn logical_snapshot(connection: &Connection) -> BTreeMap<String, Vec<Vec<u8>>> {
    let tables = connection
        .prepare(
            "SELECT name FROM sqlite_schema
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let mut snapshot = BTreeMap::new();
    for table in tables {
        let quoted_table = table.replace('"', "\"\"");
        let mut statement = connection
            .prepare(&format!("SELECT * FROM \"{quoted_table}\""))
            .unwrap();
        let column_count = statement.column_count();
        let mut query = statement.query([]).unwrap();
        let mut encoded_rows = Vec::new();
        while let Some(row) = query.next().unwrap() {
            let mut encoded = Vec::new();
            for index in 0..column_count {
                match row.get_ref(index).unwrap() {
                    ValueRef::Null => encoded.push(0),
                    ValueRef::Integer(value) => {
                        encoded.push(1);
                        encoded.extend_from_slice(&value.to_be_bytes());
                    }
                    ValueRef::Real(value) => {
                        encoded.push(2);
                        encoded.extend_from_slice(&value.to_bits().to_be_bytes());
                    }
                    ValueRef::Text(value) => {
                        encoded.push(3);
                        encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
                        encoded.extend_from_slice(value);
                    }
                    ValueRef::Blob(value) => {
                        encoded.push(4);
                        encoded.extend_from_slice(&(value.len() as u64).to_be_bytes());
                        encoded.extend_from_slice(value);
                    }
                }
            }
            encoded_rows.push(encoded);
        }
        encoded_rows.sort();
        snapshot.insert(table, encoded_rows);
    }
    snapshot
}

fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    PathBuf::from(value)
}

#[test]
fn online_backup_captures_committed_wal_and_verifies() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, installation_id, source_path) = source_store(root.path());
    let wal_path = append_suffix(&source_path, "-wal");
    assert!(fs::metadata(wal_path).unwrap().len() > 0);
    let backup_path = root.path().join("backups/verified.db");

    store
        .backup_to_verified(&backup_path, &installation_id)
        .unwrap();

    SqliteTaskStore::verify_backup(&backup_path, &installation_id).unwrap();
    let backup = open_read_only_database(&backup_path).unwrap();
    let markers = backup
        .query_row(
            "SELECT COUNT(*) FROM ledger_receipts
             WHERE idempotency_key = 'backup-marker'",
            [],
            |row| row.get::<_, u32>(0),
        )
        .unwrap();
    assert_eq!(markers, 1);
    assert!(!append_suffix(&backup_path, "-wal").exists());
    assert!(!append_suffix(&backup_path, "-shm").exists());
    assert!(!append_suffix(&backup_path, "-journal").exists());
}

#[test]
fn restore_to_new_path_preserves_all_logical_rows() {
    let root = tempfile::tempdir().unwrap();
    let (mut source, installation_id, _) = source_store(root.path());
    let backup_path = root.path().join("backups/verified.db");
    source
        .backup_to_verified(&backup_path, &installation_id)
        .unwrap();
    let before = logical_snapshot(source.connection());
    let restored_path = root.path().join("restored/state.db");

    let restored =
        SqliteTaskStore::restore_to_new_path(&backup_path, &restored_path, &installation_id)
            .unwrap();

    let after = logical_snapshot(restored.connection());
    assert!(before == after, "restored database differs logically");
    assert_eq!(restored.path(), Some(restored_path.as_path()));
}

#[test]
fn verification_rejects_corrupt_or_incompatible_backups() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, installation_id, _) = source_store(root.path());
    let backup_path = root.path().join("backups/verified.db");
    store
        .backup_to_verified(&backup_path, &installation_id)
        .unwrap();

    let truncated = root.path().join("backups/truncated.db");
    copy_private(&backup_path, &truncated);
    OpenOptions::new()
        .write(true)
        .open(&truncated)
        .unwrap()
        .set_len(64)
        .unwrap();
    assert!(SqliteTaskStore::verify_backup(&truncated, &installation_id).is_err());

    let orphaned = root.path().join("backups/orphaned.db");
    copy_private(&backup_path, &orphaned);
    let connection = Connection::open(&orphaned).unwrap();
    connection
        .execute_batch(
            "PRAGMA foreign_keys = OFF;
             INSERT INTO outbox(
                 delivery_id, task_id, event_id, delivery_kind, payload_json, state,
                 attempt, next_attempt_at_ms, lease_owner, lease_expires_at_ms,
                 created_at_ms, delivered_at_ms
             ) VALUES (
                 'orphan-delivery', 'missing-task', 'missing-event',
                 'runtime_start', '{}', 'pending', 0, 0, NULL, NULL, 0, NULL
             );",
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        SqliteTaskStore::verify_backup(&orphaned, &installation_id),
        Err(StoreError::Corrupt { message }) if message.contains("foreign_key_check")
    ));

    let newer = root.path().join("backups/newer.db");
    copy_private(&backup_path, &newer);
    let connection = Connection::open(&newer).unwrap();
    connection
        .execute(
            "INSERT INTO schema_migrations(version, checksum, applied_at_ms)
             VALUES (?1, 'future', 0)",
            [schema::CURRENT_SCHEMA_VERSION + 1],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        SqliteTaskStore::verify_backup(&newer, &installation_id),
        Err(StoreError::NewerSchema { .. })
    ));

    let checksum = root.path().join("backups/checksum.db");
    copy_private(&backup_path, &checksum);
    let connection = Connection::open(&checksum).unwrap();
    connection
        .execute(
            "UPDATE schema_migrations SET checksum = 'changed' WHERE version = 1",
            [],
        )
        .unwrap();
    drop(connection);
    assert!(matches!(
        SqliteTaskStore::verify_backup(&checksum, &installation_id),
        Err(StoreError::MigrationChecksum { version: 1 })
    ));

    assert!(matches!(
        SqliteTaskStore::verify_backup(&backup_path, &InstallationId::new()),
        Err(StoreError::LedgerConflict { .. })
    ));
}

#[test]
fn backup_and_restore_never_replace_existing_destination() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, installation_id, _) = source_store(root.path());
    let backup_path = root.path().join("backups/verified.db");
    store
        .backup_to_verified(&backup_path, &installation_id)
        .unwrap();
    let occupied = root.path().join("occupied.db");
    let sentinel = b"occupied destination";
    fs::write(&occupied, sentinel).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&occupied, fs::Permissions::from_mode(0o600)).unwrap();
    }

    assert!(matches!(
        store.backup_to_verified(&occupied, &installation_id),
        Err(StoreError::UnsafePath { .. })
    ));
    assert!(matches!(
        SqliteTaskStore::restore_to_new_path(&backup_path, &occupied, &installation_id),
        Err(StoreError::UnsafePath { .. })
    ));
    assert_eq!(fs::read(&occupied).unwrap(), sentinel);
}

#[test]
fn restore_migrates_a_verified_v1_backup_before_publication() {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    }
    let backup_path = root.path().join("gateway-v1.db");
    let installation_id = InstallationId::new();
    let actor_id = ActorId::new();
    let task_id = TaskId::new();
    let mut correlation = Correlation::new(installation_id.clone());
    correlation.actor_id = Some(actor_id.clone());
    correlation.task_id = Some(task_id.clone());
    let event = TaskEventEnvelope {
        header: ContractHeader::new(ContractSchema::TaskEvent, MessageId::new(), 1, correlation),
        task_id: task_id.clone(),
        revision: 1,
        event: TaskEvent::TaskSubmitted {
            intent_digest: Digest::parse("a".repeat(64)).unwrap(),
            target: TargetRef {
                kind: BoundedName::new("local").unwrap(),
                authority: BoundedName::new("test").unwrap(),
                identifier: BoundedOpaque::new("target").unwrap(),
            },
        },
    };
    {
        let mut connection = Connection::open(&backup_path).unwrap();
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .unwrap();
        schema::migrate_to_for_test(&mut connection, 1).unwrap();
        connection
            .execute(
                "INSERT INTO tasks(
                     task_id, owner_actor_id, target_ref, revision, state,
                     snapshot_json, created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, '{}', 1, 'submitted', '{}', 100, 100)",
                params![task_id.as_str(), actor_id.as_str()],
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO task_events(
                     event_id, task_id, revision, event_type, schema_version,
                     payload_json, occurred_at_ms, causation_id, correlation_id
                 ) VALUES (?1, ?2, 1, 'task_submitted', 1, ?3, 100, NULL, NULL)",
                params![
                    event.header.message_id.as_str(),
                    task_id.as_str(),
                    serde_json::to_string(&event).unwrap()
                ],
            )
            .unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&backup_path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    SqliteTaskStore::verify_backup(&backup_path, &installation_id).unwrap();
    let restored_path = root.path().join("restored/state.db");
    let restored =
        SqliteTaskStore::restore_to_new_path(&backup_path, &restored_path, &installation_id)
            .unwrap();

    let version = restored
        .connection()
        .query_row("SELECT MAX(version) FROM schema_migrations", [], |row| {
            row.get::<_, u32>(0)
        })
        .unwrap();
    let stored_identity = restored
        .connection()
        .query_row(
            "SELECT installation_id FROM gateway_identity WHERE singleton = 1",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap();
    assert_eq!(version, schema::CURRENT_SCHEMA_VERSION);
    assert_eq!(stored_identity, installation_id.as_str());
}
