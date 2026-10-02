//! SQLite connection policy and private-path validation.

use std::fs::{self, OpenOptions};
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use cosh_gateway_contracts::ids::InstallationId;
use cosh_gateway_contracts::task::TaskEventEnvelope;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};

use super::{schema, StoreError};

/// Single-writer SQLite Task store configured for local durable operation.
pub struct SqliteTaskStore {
    connection: Connection,
    path: Option<PathBuf>,
}

impl SqliteTaskStore {
    /// Creates and validates the private parent for a future database open.
    ///
    /// This is useful when another fail-closed durable sink must be admitted
    /// beside the database before the store connection itself is opened.
    ///
    /// # Errors
    ///
    /// Returns when any path component, owner, permission, or existing file is unsafe.
    pub fn prepare_path(path: impl AsRef<Path>) -> Result<(), StoreError> {
        prepare_private_parent(path.as_ref()).map(|_| ())
    }

    /// Opens or creates a private local database and applies checked migrations.
    ///
    /// # Errors
    ///
    /// Returns a fail-closed error for unsafe paths, unsupported migrations,
    /// corrupt storage, or SQLite configuration failures.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StoreError> {
        let path = path.as_ref();
        let database_exists = path.try_exists().map_err(|error| StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: format!("inspect database path: {error}"),
        })?;
        prepare_private_path(path)?;
        validate_companion_files(path)?;
        if database_exists {
            let preflight = Connection::open_with_flags(
                path,
                OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )?;
            configure_read_only(&preflight)?;
            schema::preflight_existing(&preflight)?;
        }
        let mut connection = Connection::open(path)?;
        configure(&connection)?;
        schema::migrate(&mut connection)?;
        let mut store = Self {
            connection,
            path: Some(path.to_path_buf()),
        };
        store.settle_legacy_runtime_start_recoveries()?;
        validate_companion_files(path)?;
        Ok(store)
    }

    /// Opens an isolated in-memory store for deterministic unit tests.
    ///
    /// # Errors
    ///
    /// Returns migration or SQLite configuration failures.
    #[cfg(test)]
    pub(crate) fn open_in_memory() -> Result<Self, StoreError> {
        let mut connection = Connection::open_in_memory()?;
        configure_in_memory(&connection)?;
        schema::migrate(&mut connection)?;
        let mut store = Self {
            connection,
            path: None,
        };
        store.settle_legacy_runtime_start_recoveries()?;
        Ok(store)
    }

    pub(super) fn connection(&self) -> &Connection {
        &self.connection
    }

    pub(super) fn connection_mut(&mut self) -> &mut Connection {
        &mut self.connection
    }

    /// Returns the durable path, or `None` for an in-memory test store.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Returns the persisted installation identity, creating it exactly once
    /// when a new database has no binding yet.
    ///
    /// A supplied identity is accepted only for the first bind or when it
    /// matches the existing value.
    ///
    /// # Errors
    ///
    /// Returns a conflict for identity substitution or corrupt stored data.
    pub fn bind_installation_id(
        &mut self,
        requested: Option<&InstallationId>,
    ) -> Result<InstallationId, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let existing = transaction
            .query_row(
                "SELECT installation_id FROM gateway_identity WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let installation_id = match existing {
            Some(value) => {
                let existing =
                    InstallationId::parse(value).map_err(|error| StoreError::Corrupt {
                        message: format!("stored installation identity is invalid: {error}"),
                    })?;
                if requested.is_some_and(|requested| requested != &existing) {
                    return Err(StoreError::LedgerConflict {
                        message: "database is bound to another installation identity".to_owned(),
                    });
                }
                existing
            }
            None => {
                let recovered = recover_installation_id(&transaction)?;
                if let (Some(requested), Some(recovered)) = (requested, recovered.as_ref()) {
                    if requested != recovered {
                        return Err(StoreError::LedgerConflict {
                            message: "existing Task history belongs to another installation"
                                .to_owned(),
                        });
                    }
                }
                let installation_id = recovered.or_else(|| requested.cloned()).unwrap_or_default();
                transaction.execute(
                    "INSERT INTO gateway_identity(singleton, installation_id) VALUES (1, ?1)",
                    params![installation_id.as_str()],
                )?;
                installation_id
            }
        };
        transaction.commit()?;
        Ok(installation_id)
    }
}

pub(super) fn recover_installation_id(
    connection: &Connection,
) -> Result<Option<InstallationId>, StoreError> {
    let mut statement =
        connection.prepare("SELECT payload_json FROM task_events ORDER BY task_id, revision")?;
    let payloads = statement.query_map([], |row| row.get::<_, String>(0))?;
    let mut recovered: Option<InstallationId> = None;
    for payload in payloads {
        let event = serde_json::from_str::<TaskEventEnvelope>(&payload?).map_err(|error| {
            StoreError::Corrupt {
                message: format!("Task event cannot recover installation identity: {error}"),
            }
        })?;
        let candidate = event.header.correlation.installation_id;
        if recovered
            .as_ref()
            .is_some_and(|recovered| recovered != &candidate)
        {
            return Err(StoreError::Corrupt {
                message: "Task history contains multiple installation identities".to_owned(),
            });
        }
        recovered = Some(candidate);
    }
    Ok(recovered)
}

fn configure(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         PRAGMA busy_timeout = 5000;
         PRAGMA trusted_schema = OFF;",
    )?;
    Ok(())
}

pub(super) fn configure_read_only(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "PRAGMA query_only = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA trusted_schema = OFF;",
    )?;
    Ok(())
}

#[cfg(test)]
fn configure_in_memory(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA synchronous = FULL;
         PRAGMA busy_timeout = 5000;
         PRAGMA trusted_schema = OFF;",
    )?;
    Ok(())
}

fn prepare_private_path(path: &Path) -> Result<(), StoreError> {
    if !path.is_absolute() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: "database path must be absolute".to_string(),
        });
    }
    let parent = path.parent().ok_or_else(|| StoreError::UnsafePath {
        path: path.to_path_buf(),
        message: "database path has no parent directory".to_string(),
    })?;
    create_private_path_components(parent)?;
    validate_private_permissions(parent, true)?;

    match fs::symlink_metadata(path) {
        Ok(_) => {
            reject_symlink_or_wrong_type(path, false)?;
            validate_private_permissions(path, false)?;
        }
        Err(error) if error.kind() == ErrorKind::NotFound => create_private_file(path)?,
        Err(error) => {
            return Err(StoreError::UnsafePath {
                path: path.to_path_buf(),
                message: format!("inspect database file: {error}"),
            });
        }
    }
    Ok(())
}

pub(super) fn prepare_new_private_file_path(path: &Path) -> Result<(), StoreError> {
    prepare_private_parent(path)?;
    match fs::symlink_metadata(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Ok(_) => Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: "destination must not already exist".to_owned(),
        }),
        Err(error) => Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: format!("inspect destination path: {error}"),
        }),
    }
}

pub(super) fn validate_existing_private_file_path(path: &Path) -> Result<(), StoreError> {
    if !path.is_absolute() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: "database path must be absolute".to_owned(),
        });
    }
    let parent = path.parent().ok_or_else(|| StoreError::UnsafePath {
        path: path.to_path_buf(),
        message: "database path has no parent directory".to_owned(),
    })?;
    validate_existing_path_components(parent)?;
    validate_private_permissions(parent, true)?;
    reject_symlink_or_wrong_type(path, false)?;
    validate_private_permissions(path, false)?;
    validate_companion_files(path)
}

fn prepare_private_parent(path: &Path) -> Result<&Path, StoreError> {
    if !path.is_absolute() {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: "database path must be absolute".to_owned(),
        });
    }
    let parent = path.parent().ok_or_else(|| StoreError::UnsafePath {
        path: path.to_path_buf(),
        message: "database path has no parent directory".to_owned(),
    })?;
    create_private_path_components(parent)?;
    validate_private_permissions(parent, true)?;
    Ok(parent)
}

fn validate_existing_path_components(path: &Path) -> Result<(), StoreError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        current.push(component.as_os_str());
        reject_symlink_or_wrong_type(&current, true)?;
    }
    Ok(())
}

fn create_private_path_components(parent: &Path) -> Result<(), StoreError> {
    let mut current = PathBuf::new();
    for component in parent.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(_) => reject_symlink_or_wrong_type(&current, true)?,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                create_private_directory(&current)?;
                reject_symlink_or_wrong_type(&current, true)?;
            }
            Err(error) => {
                return Err(StoreError::UnsafePath {
                    path: current,
                    message: format!("inspect state path component: {error}"),
                });
            }
        }
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), StoreError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|error| StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: format!("create private state directory: {error}"),
        })
}

fn create_private_file(path: &Path) -> Result<(), StoreError> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
        .open(path)
        .map(|_| ())
        .map_err(|error| StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: format!("create private database file: {error}"),
        })
}

fn validate_companion_files(path: &Path) -> Result<(), StoreError> {
    for suffix in ["-wal", "-shm"] {
        let mut companion = path.as_os_str().to_os_string();
        companion.push(suffix);
        let companion = PathBuf::from(companion);
        match fs::symlink_metadata(&companion) {
            Ok(_) => {
                reject_symlink_or_wrong_type(&companion, false)?;
                validate_private_permissions(&companion, false)?;
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(StoreError::UnsafePath {
                    path: companion,
                    message: format!("inspect SQLite companion file: {error}"),
                });
            }
        }
    }
    Ok(())
}

fn reject_symlink_or_wrong_type(path: &Path, directory: bool) -> Result<(), StoreError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| StoreError::UnsafePath {
        path: path.to_path_buf(),
        message: format!("inspect path: {error}"),
    })?;
    let file_type = metadata.file_type();
    if file_type.is_symlink()
        || (directory && !file_type.is_dir())
        || (!directory && !file_type.is_file())
    {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: if directory {
                "expected a real directory, not a symlink or special file"
            } else {
                "expected a regular file, not a symlink or special file"
            }
            .to_string(),
        });
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_permissions(path: &Path, directory: bool) -> Result<(), StoreError> {
    use std::os::unix::fs::PermissionsExt;

    let mode = fs::symlink_metadata(path)
        .map_err(|error| StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: format!("inspect private permissions: {error}"),
        })?
        .permissions()
        .mode();
    if mode & 0o077 != 0 {
        return Err(StoreError::UnsafePath {
            path: path.to_path_buf(),
            message: if directory {
                "state directory grants group or other permissions"
            } else {
                "database file grants group or other permissions"
            }
            .to_string(),
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_permissions(_path: &Path, _directory: bool) -> Result<(), StoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot_directory(path: &Path) -> Vec<(String, Vec<u8>)> {
        let mut snapshot = fs::read_dir(path)
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.file_name().to_string_lossy().into_owned(),
                    fs::read(entry.path()).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        snapshot.sort_by(|left, right| left.0.cmp(&right.0));
        snapshot
    }

    fn create_delete_journal_database(path: &Path) {
        drop(SqliteTaskStore::open(path).unwrap());
        let connection = Connection::open(path).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode = DELETE;")
            .unwrap();
        let journal: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "delete");
    }

    #[test]
    fn relative_database_path_is_rejected() {
        assert!(matches!(
            SqliteTaskStore::open("relative/state.db"),
            Err(StoreError::UnsafePath { .. })
        ));
    }

    #[test]
    fn durable_store_uses_wal_full_and_foreign_keys() {
        let directory = tempfile::tempdir().unwrap();
        let store = SqliteTaskStore::open(directory.path().join("gateway/state.db")).unwrap();
        let journal: String = store
            .connection()
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        let synchronous: u32 = store
            .connection()
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        let foreign_keys: u32 = store
            .connection()
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert_eq!(journal, "wal");
        assert_eq!(synchronous, 2);
        assert_eq!(foreign_keys, 1);
    }

    #[test]
    fn newer_schema_preflight_is_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let state_directory = directory.path().join("gateway");
        let path = state_directory.join("state.db");
        create_delete_journal_database(&path);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "INSERT INTO schema_migrations(version, checksum, applied_at_ms)
                 VALUES (?1, 'future', 0)",
                [schema::CURRENT_SCHEMA_VERSION + 1],
            )
            .unwrap();
        drop(connection);
        let before = snapshot_directory(&state_directory);

        let error = match SqliteTaskStore::open(&path) {
            Ok(_) => panic!("future schema must be rejected"),
            Err(error) => error,
        };

        assert!(matches!(error, StoreError::NewerSchema { .. }));
        assert_eq!(snapshot_directory(&state_directory), before);
    }

    #[test]
    fn checksum_preflight_is_read_only() {
        let directory = tempfile::tempdir().unwrap();
        let state_directory = directory.path().join("gateway");
        let path = state_directory.join("state.db");
        create_delete_journal_database(&path);
        let connection = Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE schema_migrations SET checksum = 'changed' WHERE version = 1",
                [],
            )
            .unwrap();
        drop(connection);
        let before = snapshot_directory(&state_directory);

        let error = match SqliteTaskStore::open(&path) {
            Ok(_) => panic!("checksum mismatch must be rejected"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            StoreError::MigrationChecksum { version: 1 }
        ));
        assert_eq!(snapshot_directory(&state_directory), before);
    }

    #[test]
    fn foreign_key_corruption_fails_before_read_write_open() {
        let directory = tempfile::tempdir().unwrap();
        let state_directory = directory.path().join("gateway");
        let path = state_directory.join("state.db");
        create_delete_journal_database(&path);
        let connection = Connection::open(&path).unwrap();
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
        let before = snapshot_directory(&state_directory);

        let error = match SqliteTaskStore::open(&path) {
            Ok(_) => panic!("foreign-key corruption must be rejected"),
            Err(error) => error,
        };

        assert!(matches!(
            error,
            StoreError::Corrupt { message }
                if message.contains("foreign_key_check")
        ));
        assert_eq!(snapshot_directory(&state_directory), before);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_database_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let actual = directory.path().join("actual.db");
        fs::write(&actual, []).unwrap();
        let link = directory.path().join("state.db");
        symlink(&actual, &link).unwrap();
        assert!(matches!(
            SqliteTaskStore::open(&link),
            Err(StoreError::UnsafePath { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn invalid_utf8_database_path_rejects_companion_symlinks() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::fs::{symlink, PermissionsExt};

        let directory = tempfile::tempdir().unwrap();
        let mut path = directory.path().to_path_buf();
        path.push(OsString::from_vec(b"state-\xff.db".to_vec()));
        fs::write(&path, []).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();

        for suffix in ["-wal", "-shm"] {
            let mut companion = path.as_os_str().to_os_string();
            companion.push(suffix);
            symlink(&path, PathBuf::from(companion)).unwrap();
        }

        assert!(matches!(
            SqliteTaskStore::open(&path),
            Err(StoreError::UnsafePath { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn insecure_existing_parent_is_rejected_without_chmod() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("gateway");
        fs::create_dir(&parent).unwrap();
        fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();

        assert!(matches!(
            SqliteTaskStore::open(parent.join("state.db")),
            Err(StoreError::UnsafePath { .. })
        ));
        assert_eq!(
            fs::symlink_metadata(&parent).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!parent.join("state.db").exists());
    }

    #[cfg(unix)]
    #[test]
    fn intermediate_symlink_is_rejected() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let actual = directory.path().join("actual");
        fs::create_dir(&actual).unwrap();
        let link = directory.path().join("linked");
        symlink(&actual, &link).unwrap();

        assert!(matches!(
            SqliteTaskStore::open(link.join("gateway/state.db")),
            Err(StoreError::UnsafePath { .. })
        ));
        assert!(!actual.join("gateway/state.db").exists());
    }

    #[cfg(unix)]
    #[test]
    fn prepared_parent_does_not_manufacture_an_empty_database() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("private/gateway/state.db");

        SqliteTaskStore::prepare_path(&path).unwrap();

        assert!(!path.exists());
        assert_eq!(
            fs::symlink_metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        SqliteTaskStore::open(&path).unwrap();
    }
}
