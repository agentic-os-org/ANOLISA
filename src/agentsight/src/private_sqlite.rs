//! Private SQLite state-file creation shared by security-sensitive stores.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use thiserror::Error;

#[derive(Debug, Error)]
pub(crate) enum PrivateSqliteError {
    /// The requested name could escape the dedicated state directory.
    #[error("invalid private SQLite database name {0:?}")]
    InvalidDatabaseName(String),
    /// A filesystem operation failed before SQLite could open safely.
    #[error("{operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    /// An existing path has unsafe type, ownership, or sharing semantics.
    #[error("unsafe private SQLite path {path}: {reason}")]
    UnsafePath { path: PathBuf, reason: &'static str },
    /// SQLite initialization or WAL configuration failed.
    #[error("configure private SQLite database: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

/// Opens one database in a process-owned 0700 directory with 0600 state files.
///
/// # Errors
///
/// Returns a typed unsafe-path, I/O, or SQLite configuration error.
pub(crate) fn open_private_connection(
    state_dir: &Path,
    database_name: &str,
) -> Result<Connection, PrivateSqliteError> {
    validate_database_name(database_name)?;
    create_private_directory(state_dir)?;
    let state_dir = fs::canonicalize(state_dir)
        .map_err(|source| io_error("resolve private directory", state_dir, source))?;

    let database = state_dir.join(database_name);
    let wal = state_dir.join(format!("{database_name}-wal"));
    let shm = state_dir.join(format!("{database_name}-shm"));
    open_or_create_private_file(&database)?;
    repair_existing_private_file(&wal)?;
    repair_existing_private_file(&shm)?;

    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_CREATE
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(&database, flags)?;
    connection.execute_batch("PRAGMA journal_mode=WAL;")?;
    connection.busy_timeout(Duration::from_millis(500))?;

    // SQLite derives new sidecar modes from the database, and this also repairs
    // sidecars left by older versions before serving any request.
    open_or_create_private_file(&database)?;
    repair_existing_private_file(&wal)?;
    repair_existing_private_file(&shm)?;
    Ok(connection)
}

fn validate_database_name(database_name: &str) -> Result<(), PrivateSqliteError> {
    let mut components = Path::new(database_name).components();
    if !matches!(components.next(), Some(Component::Normal(_))) || components.next().is_some() {
        return Err(PrivateSqliteError::InvalidDatabaseName(
            database_name.to_string(),
        ));
    }
    Ok(())
}

fn create_private_directory(path: &Path) -> Result<(), PrivateSqliteError> {
    reject_shared_directory(path)?;
    let mut builder = DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .map_err(|source| io_error("create directory", path, source))?;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC);
    let directory = options
        .open(path)
        .map_err(|source| io_error("open directory", path, source))?;
    validate_owner_and_type(&directory, path, true)?;
    directory
        .set_permissions(fs::Permissions::from_mode(0o700))
        .map_err(|source| io_error("set directory permissions", path, source))?;
    Ok(())
}

fn reject_shared_directory(path: &Path) -> Result<(), PrivateSqliteError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error("inspect state directory", path, source)),
    };
    let shared_sticky = metadata.file_type().is_dir()
        && metadata.mode() & 0o1000 != 0
        && metadata.mode() & 0o002 != 0;
    if shared_sticky || path.parent().is_none() {
        return Err(PrivateSqliteError::UnsafePath {
            path: path.to_path_buf(),
            reason: "shared directory cannot be repurposed as private state",
        });
    }
    Ok(())
}

fn open_or_create_private_file(path: &Path) -> Result<(), PrivateSqliteError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    let file = options
        .open(path)
        .map_err(|source| io_error("open database file", path, source))?;
    validate_owner_and_type(&file, path, false)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|source| io_error("set database permissions", path, source))?;
    Ok(())
}

fn repair_existing_private_file(path: &Path) -> Result<(), PrivateSqliteError> {
    let mut options = OpenOptions::new();
    options
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    match options.open(path) {
        Ok(file) => {
            validate_owner_and_type(&file, path, false)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))
                .map_err(|source| io_error("set sidecar permissions", path, source))?;
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(io_error("open sidecar file", path, source)),
    }
}

fn validate_owner_and_type(
    file: &File,
    path: &Path,
    directory: bool,
) -> Result<(), PrivateSqliteError> {
    let metadata = file
        .metadata()
        .map_err(|source| io_error("inspect path", path, source))?;
    let expected_type = if directory {
        metadata.file_type().is_dir()
    } else {
        metadata.file_type().is_file()
    };
    if !expected_type {
        return Err(PrivateSqliteError::UnsafePath {
            path: path.to_path_buf(),
            reason: if directory {
                "not a directory"
            } else {
                "not a regular file"
            },
        });
    }
    // SAFETY: `geteuid` has no preconditions and does not dereference memory.
    let effective_uid = unsafe { libc::geteuid() };
    if metadata.uid() != effective_uid {
        return Err(PrivateSqliteError::UnsafePath {
            path: path.to_path_buf(),
            reason: "owner does not match the effective user",
        });
    }
    Ok(())
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> PrivateSqliteError {
    PrivateSqliteError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    use super::open_private_connection;

    #[test]
    fn repairs_directory_database_and_sidecar_modes() {
        let root = std::env::temp_dir().join(format!(
            "agentsight-private-sqlite-{}",
            uuid::Uuid::new_v4()
        ));
        let state = root.join("state");
        let first =
            open_private_connection(&state, "security.db").expect("private connection should open");
        first
            .execute_batch("CREATE TABLE fixture (value INTEGER); INSERT INTO fixture VALUES (1);")
            .expect("fixture write should create WAL sidecars");
        let database = state.join("security.db");
        let wal = state.join("security.db-wal");
        let shm = state.join("security.db-shm");
        for path in [&state, &database, &wal, &shm] {
            let mode = if path == &state { 0o777 } else { 0o666 };
            fs::set_permissions(path, fs::Permissions::from_mode(mode))
                .expect("fixture mode should change");
        }

        let repaired = open_private_connection(&state, "security.db")
            .expect("existing private connection should reopen");

        assert_eq!(
            fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o700
        );
        for path in [&database, &wal, &shm] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        drop(repaired);
        drop(first);
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn rejects_a_database_symlink() {
        let root = std::env::temp_dir().join(format!(
            "agentsight-private-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        let state = root.join("state");
        fs::create_dir_all(&state).expect("fixture directory should exist");
        let target = root.join("target.db");
        fs::write(&target, []).expect("fixture target should exist");
        symlink(&target, state.join("security.db")).expect("fixture symlink should exist");

        let error = open_private_connection(&state, "security.db")
            .expect_err("database symlinks must be rejected");

        assert!(error.to_string().contains("security.db"));
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn rejects_a_state_directory_symlink() {
        let root = std::env::temp_dir().join(format!(
            "agentsight-private-dir-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        let target = root.join("target");
        fs::create_dir_all(&target).expect("fixture target should exist");
        let state = root.join("state");
        symlink(&target, &state).expect("fixture symlink should exist");

        let error = open_private_connection(&state, "security.db")
            .expect_err("state-directory symlinks must be rejected");

        assert!(error.to_string().contains("state"));
        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[test]
    fn rejects_a_shared_sticky_directory_without_chmodding_it() {
        let state =
            std::env::temp_dir().join(format!("agentsight-shared-state-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&state).expect("fixture directory should exist");
        fs::set_permissions(&state, fs::Permissions::from_mode(0o1777))
            .expect("fixture mode should change");

        let error = open_private_connection(&state, "security.db")
            .expect_err("shared directories must not be repurposed as private state");

        assert!(error.to_string().contains("shared"));
        assert_eq!(
            fs::metadata(&state).unwrap().permissions().mode() & 0o1777,
            0o1777
        );
        fs::remove_dir(state).expect("fixture directory should be removed");
    }
}
