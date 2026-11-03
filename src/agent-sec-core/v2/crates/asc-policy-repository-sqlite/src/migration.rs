//! Ordered schema upgrades before current-format records are decoded.
use crate::RepositoryError;
use crate::table::POLICY_TABLES;
use asc_policy_repository::StoreError;
use rusqlite::{Connection, Transaction, TransactionBehavior};

/// Each entry upgrades version N to N+1, including any persisted JSON conversion.
/// Append steps; never reorder or change a released step. The runner owns commit
/// and `user_version`, so a step must not commit or perform external I/O.
type Migration = fn(&Transaction<'_>) -> Result<(), RepositoryError>;

const MIGRATIONS: &[Migration] = &[initialize_v1];

/// Applies all missing steps atomically; rejects databases newer than this binary.
/// # Errors
/// Rejects unsupported versions, nonempty unversioned databases and failed upgrades.
pub(crate) fn migrate(connection: &mut Connection) -> Result<(), RepositoryError> {
    apply_migrations(connection, MIGRATIONS)
}

fn apply_migrations(
    connection: &mut Connection,
    migrations: &[Migration],
) -> Result<(), RepositoryError> {
    let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let version: i64 = transaction.pragma_query_value(None, "user_version", |r| r.get(0))?;
    let current = usize::try_from(version).map_err(|_| StoreError::Incompatible)?;
    if current > migrations.len() {
        return Err(StoreError::Incompatible.into());
    }
    for (index, step) in migrations.iter().enumerate().skip(current) {
        step(&transaction)?;
        let next = i64::try_from(index + 1).map_err(|_| StoreError::Incompatible)?;
        transaction.pragma_update(None, "user_version", next)?;
    }
    transaction.commit()?;
    Ok(())
}

fn initialize_v1(transaction: &Transaction<'_>) -> Result<(), RepositoryError> {
    let count: i64 =
        transaction.query_row("SELECT count(*) FROM sqlite_schema", [], |r| r.get(0))?;
    if count != 0 {
        return Err(StoreError::Incompatible.into());
    }
    for table in POLICY_TABLES {
        transaction.execute_batch(&table.create_table_sql())?;
        for index in table.indexes {
            transaction.execute_batch(&table.create_index_sql(index))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(connection: &Connection) -> i64 {
        connection
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn current_database_is_not_initialized_again() {
        let mut connection = Connection::open_in_memory().unwrap();
        migrate(&mut connection).unwrap();
        assert_eq!(version(&connection), 1);
        connection
            .execute("INSERT INTO policies VALUES ('kept',1,NULL)", [])
            .unwrap();
        migrate(&mut connection).unwrap();
        assert_eq!(version(&connection), 1);
        assert_eq!(
            connection
                .query_row("SELECT policy_id FROM policies", [], |r| r
                    .get::<_, String>(0))
                .unwrap(),
            "kept"
        );
    }

    #[test]
    fn initialized_tables_enforce_lifecycle_and_identity_constraints() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        migrate(&mut connection).unwrap();
        connection.execute_batch(
            "INSERT INTO scopes VALUES ('s', '{}', 'ACTIVE', NULL, 0);
             INSERT INTO bindings (binding_id, scope_id, policy_id, boot_id, pid_namespace,
                 pid, start_time, binding_revision, spec_json, phase, status_version, deployments_json)
             VALUES ('b', 's', 'p', 'boot', 'ns', 1, '1', 1, '{}', 'PENDING_APPLY', 1, '[]')",
        ).unwrap();
        for sql in [
            "UPDATE scopes SET discovery_stopped=1",
            "UPDATE bindings SET last_write_id='incomplete-receipt'",
            "INSERT INTO bindings SELECT 'duplicate', scope_id, policy_id, boot_id, pid_namespace,
                pid, start_time, binding_revision, spec_json, phase, error_json, status_version,
                deployments_json, last_write_id, last_write_digest, last_write_result_json FROM bindings",
            "DELETE FROM scopes",
        ] {
            let error = connection.execute(sql, []).expect_err("constraint violation");
            assert_eq!(error.sqlite_error_code(), Some(rusqlite::ErrorCode::ConstraintViolation));
        }
    }

    #[test]
    fn unknown_versions_and_nonempty_unversioned_databases_remain_untouched() {
        for original_version in [-1, 0, 2] {
            let mut connection = Connection::open_in_memory().unwrap();
            connection
                .execute_batch(
                    "CREATE TABLE existing(value TEXT); INSERT INTO existing VALUES ('kept')",
                )
                .unwrap();
            connection
                .pragma_update(None, "user_version", original_version)
                .unwrap();
            assert!(matches!(
                migrate(&mut connection),
                Err(RepositoryError::Storage(StoreError::Incompatible))
            ));
            assert_eq!(version(&connection), original_version);
            assert_eq!(
                connection
                    .query_row("SELECT value FROM existing", [], |r| r.get::<_, String>(0))
                    .unwrap(),
                "kept"
            );
            assert_eq!(
                connection
                    .query_row("SELECT count(*) FROM sqlite_schema", [], |r| r
                        .get::<_, i64>(0))
                    .unwrap(),
                1
            );
        }
    }

    fn test_v1(transaction: &Transaction<'_>) -> Result<(), RepositoryError> {
        transaction.execute_batch(
            "CREATE TABLE sample(value TEXT); INSERT INTO sample VALUES ('original')",
        )?;
        Ok(())
    }

    fn test_v2(transaction: &Transaction<'_>) -> Result<(), RepositoryError> {
        transaction.execute_batch(
            "ALTER TABLE sample ADD COLUMN migrated INTEGER NOT NULL DEFAULT 0;
            UPDATE sample SET value='converted',migrated=1",
        )?;
        Ok(())
    }

    fn failing_v2(transaction: &Transaction<'_>) -> Result<(), RepositoryError> {
        test_v2(transaction)?;
        Err(StoreError::Io.into())
    }

    #[test]
    fn failed_upgrade_rolls_back_schema_data_and_version_before_retry() {
        let mut connection = Connection::open_in_memory().unwrap();
        apply_migrations(&mut connection, &[test_v1]).unwrap();
        assert!(matches!(
            apply_migrations(&mut connection, &[test_v1, failing_v2]),
            Err(RepositoryError::Storage(StoreError::Io))
        ));
        assert!(connection.is_autocommit());
        assert_eq!(version(&connection), 1);
        assert_eq!(
            connection
                .query_row("SELECT value FROM sample", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "original"
        );
        assert!(connection.prepare("SELECT migrated FROM sample").is_err());
        apply_migrations(&mut connection, &[test_v1, test_v2]).unwrap();
        assert_eq!(version(&connection), 2);
        assert_eq!(
            connection
                .query_row("SELECT value,migrated FROM sample", [], |r| Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?
                )))
                .unwrap(),
            ("converted".into(), 1)
        );
        // A second open must not rerun ALTER TABLE or any data conversion.
        apply_migrations(&mut connection, &[test_v1, test_v2]).unwrap();
        assert_eq!(version(&connection), 2);
    }
}
