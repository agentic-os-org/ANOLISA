use std::{path::Path, thread, time::Duration};

use rusqlite::{Connection, OpenFlags};

use crate::LifecycleError;

/// Controls whether opening a database may create or modify it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionMode {
    /// Open for reads and writes, creating the database when absent.
    ReadWriteCreate,
    /// Open an existing database for reads and writes without creating it.
    ReadWriteExisting,
    /// Open an existing database without allowing writes or creation.
    ReadOnlyExisting,
}

/// Common connection settings shared by AgentSight SQLite stores.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectionOptions {
    /// Access mode for the database.
    pub mode: ConnectionMode,
    /// Time spent waiting for transient SQLite locks.
    pub busy_timeout: Duration,
    /// Whether writable connections use WAL journal mode.
    pub enable_wal: bool,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            mode: ConnectionMode::ReadWriteCreate,
            busy_timeout: Duration::from_millis(500),
            enable_wal: true,
        }
    }
}

/// Opens a SQLite database with explicit lifecycle behavior.
///
/// # Errors
///
/// Returns an error when an existing-only mode cannot find the database, a
/// create-mode parent directory cannot be created, or SQLite rejects the settings.
pub fn open_connection(
    path: &Path,
    options: ConnectionOptions,
) -> Result<Connection, LifecycleError> {
    let connection = match options.mode {
        ConnectionMode::ReadWriteCreate => {
            if let Some(parent) = path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent).map_err(|source| LifecycleError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
            Connection::open(path)?
        }
        ConnectionMode::ReadWriteExisting => {
            if !path.is_file() {
                return Err(LifecycleError::DatabaseMissing(path.to_path_buf()));
            }
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?
        }
        ConnectionMode::ReadOnlyExisting => {
            if !path.is_file() {
                return Err(LifecycleError::DatabaseMissing(path.to_path_buf()));
            }
            Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?
        }
    };

    connection.busy_timeout(options.busy_timeout)?;
    if options.enable_wal && options.mode != ConnectionMode::ReadOnlyExisting {
        connection.execute_batch("PRAGMA journal_mode=WAL;")?;
    }

    Ok(connection)
}

/// Result of a truncating WAL checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointOutcome {
    /// All WAL frames were checkpointed and the WAL was truncated.
    Completed,
    /// Another connection prevented the WAL from being truncated.
    Busy,
}

/// Flushes WAL frames and reports whether lock contention prevented truncation.
///
/// # Errors
///
/// Returns an error when SQLite cannot execute the checkpoint query.
pub fn checkpoint_truncate(connection: &Connection) -> Result<CheckpointOutcome, LifecycleError> {
    let busy: i32 =
        connection.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))?;
    Ok(if busy == 0 {
        CheckpointOutcome::Completed
    } else {
        CheckpointOutcome::Busy
    })
}

/// Returns whether an error represents transient SQLite lock contention.
pub fn is_retryable_lock(error: &LifecycleError) -> bool {
    matches!(
        error,
        LifecycleError::Sqlite(rusqlite::Error::SqliteFailure(code, _))
            if matches!(
                code.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

/// Retries an operation after transient SQLite lock failures.
///
/// # Errors
///
/// Returns immediately for non-lock errors, or [`LifecycleError::RetryExhausted`]
/// after all configured delays have been used.
pub fn retry_with_backoff<T>(
    mut operation: impl FnMut() -> Result<T, LifecycleError>,
    backoff: &[Duration],
) -> Result<T, LifecycleError> {
    let mut attempts = 1usize;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if is_retryable_lock(&error) => {
                let Some(delay) = backoff.get(attempts - 1) else {
                    return Err(LifecycleError::RetryExhausted {
                        attempts,
                        source: Box::new(error),
                    });
                };
                thread::sleep(*delay);
                attempts += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use super::*;

    fn test_path(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system time must follow Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agentsight-{name}-{}-{nonce}.db",
            std::process::id()
        ))
    }

    #[test]
    fn writable_open_creates_database_and_enables_wal() {
        let path = test_path("connection");
        let connection = open_connection(&path, ConnectionOptions::default()).unwrap();
        let journal: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        drop(connection);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn existing_only_modes_never_create_missing_database_or_parent() {
        for mode in [
            ConnectionMode::ReadWriteExisting,
            ConnectionMode::ReadOnlyExisting,
        ] {
            let parent = test_path("missing-parent");
            let path = parent.join("database.db");
            let result = open_connection(
                &path,
                ConnectionOptions {
                    mode,
                    ..ConnectionOptions::default()
                },
            );
            assert!(matches!(result, Err(LifecycleError::DatabaseMissing(p)) if p == path));
            assert!(!parent.exists());
        }
    }

    #[test]
    fn read_write_existing_is_writable_and_can_enable_wal() {
        let path = test_path("existing");
        let initial = open_connection(
            &path,
            ConnectionOptions {
                enable_wal: false,
                ..ConnectionOptions::default()
            },
        )
        .unwrap();
        drop(initial);

        let connection = open_connection(
            &path,
            ConnectionOptions {
                mode: ConnectionMode::ReadWriteExisting,
                ..ConnectionOptions::default()
            },
        )
        .unwrap();
        connection
            .execute_batch("CREATE TABLE writable (value INTEGER NOT NULL);")
            .unwrap();
        let journal: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");

        drop(connection);
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn retry_stops_after_configured_delays() {
        let mut attempts = 0;
        let result = retry_with_backoff(
            || {
                attempts += 1;
                Err::<(), _>(LifecycleError::Sqlite(rusqlite::Error::SqliteFailure(
                    rusqlite::ffi::Error::new(5),
                    None,
                )))
            },
            &[Duration::ZERO, Duration::ZERO],
        );
        assert!(matches!(
            result,
            Err(LifecycleError::RetryExhausted { attempts: 3, .. })
        ));
        assert_eq!(attempts, 3);
    }
}
