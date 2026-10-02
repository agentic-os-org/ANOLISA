fn migrate_through(connection: &mut Connection, target_version: u32) -> Result<(), StoreError> {
    validate_schema_history(connection, target_version)?;
    validate_integrity(connection)?;

    connection.execute_batch(
        "BEGIN IMMEDIATE;
         CREATE TABLE IF NOT EXISTS schema_migrations (
             version INTEGER PRIMARY KEY NOT NULL CHECK (version > 0),
             checksum TEXT NOT NULL,
             applied_at_ms INTEGER NOT NULL CHECK (applied_at_ms >= 0)
         ) STRICT;
         COMMIT;",
    )?;

    for migration in MIGRATIONS
        .iter()
        .filter(|migration| migration.version <= target_version)
    {
        let existing = connection
            .query_row(
                "SELECT checksum FROM schema_migrations WHERE version = ?1",
                params![migration.version],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        match existing {
            Some(checksum) if checksum == migration.checksum => continue,
            Some(_) => {
                return Err(StoreError::MigrationChecksum {
                    version: migration.version,
                });
            }
            None => apply_migration(connection, migration)?,
        }
    }

    validate_integrity(connection)
}

fn validate_schema_history(
    connection: &Connection,
    supported_version: u32,
) -> Result<(), StoreError> {
    let has_migration_table = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name = 'schema_migrations'
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    if !has_migration_table {
        let object_count = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        if object_count == 0 {
            return Ok(());
        }
        return Err(StoreError::Corrupt {
            message: "database objects exist without schema migration history".to_owned(),
        });
    }

    let found = connection.query_row(
        "SELECT COALESCE(MAX(version), 0) FROM schema_migrations",
        [],
        |row| row.get::<_, u32>(0),
    )?;
    if found > supported_version {
        return Err(StoreError::NewerSchema {
            found,
            supported: supported_version,
        });
    }

    let mut statement =
        connection.prepare("SELECT version, checksum FROM schema_migrations ORDER BY version")?;
    let rows = statement.query_map([], |row| {
        Ok((row.get::<_, u32>(0)?, row.get::<_, String>(1)?))
    })?;
    let history = rows.collect::<Result<Vec<_>, _>>()?;

    for (index, (version, checksum)) in history.iter().enumerate() {
        let expected_version = u32::try_from(index + 1).map_err(|_| StoreError::Corrupt {
            message: "schema migration history exceeds supported integer range".to_owned(),
        })?;
        if *version != expected_version {
            return Err(StoreError::Corrupt {
                message: format!(
                    "schema migration history is not contiguous: expected version {expected_version}, found {version}"
                ),
            });
        }
        let migration = MIGRATIONS
            .iter()
            .find(|migration| migration.version == *version)
            .ok_or(StoreError::NewerSchema {
                found: *version,
                supported: supported_version,
            })?;
        if checksum != migration.checksum {
            return Err(StoreError::MigrationChecksum { version: *version });
        }
    }

    if history.is_empty() {
        let object_count = connection.query_row(
            "SELECT COUNT(*) FROM sqlite_schema
             WHERE name NOT LIKE 'sqlite_%' AND name != 'schema_migrations'",
            [],
            |row| row.get::<_, u64>(0),
        )?;
        if object_count != 0 {
            return Err(StoreError::Corrupt {
                message: "database objects exist without recorded migrations".to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_integrity(connection: &Connection) -> Result<(), StoreError> {
    let integrity: String = connection.query_row("PRAGMA quick_check(1)", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(StoreError::Corrupt {
            message: format!("SQLite quick_check failed: {integrity}"),
        });
    }
    validate_foreign_keys(connection)?;
    Ok(())
}

fn validate_foreign_keys(connection: &Connection) -> Result<(), StoreError> {
    let violation = connection
        .query_row("PRAGMA foreign_key_check", [], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .optional()?;
    if let Some((table, row_id, parent, constraint)) = violation {
        return Err(StoreError::Corrupt {
            message: format!(
                "SQLite foreign_key_check failed: table={table}, rowid={row_id:?}, parent={parent}, constraint={constraint}"
            ),
        });
    }
    Ok(())
}

fn apply_migration(connection: &mut Connection, migration: &Migration) -> Result<(), StoreError> {
    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    transaction.execute_batch(migration.sql)?;
    validate_foreign_keys(&transaction)?;
    record_migration(&transaction, migration)?;
    transaction.commit()?;
    Ok(())
}

fn record_migration(
    transaction: &Transaction<'_>,
    migration: &Migration,
) -> Result<(), StoreError> {
    transaction.execute(
        "INSERT INTO schema_migrations(version, checksum, applied_at_ms)
         VALUES (?1, ?2, CAST(unixepoch('subsec') * 1000 AS INTEGER))",
        params![migration.version, migration.checksum],
    )?;
    Ok(())
}
