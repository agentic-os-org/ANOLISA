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
    reject_shared_directory(state_dir)?;
    let (storage_root, _) = split_private_directory(state_dir)?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(storage_root)
        .map_err(|source| io_error("create storage root", storage_root, source))?;
    // Canonicalize only the user-supplied storage root so the protected state
    // directory name itself stays literal for O_NOFOLLOW and SQLITE_OPEN_NOFOLLOW.
    let state_dir = resolve_private_directory(state_dir)?;
    create_private_directory(&state_dir)?;

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

    // SQLite derives new sidecar modes from the database. The checks above
    // also repair retained files before SQLite holds its POSIX locks.
    Ok(connection)
}

#[cfg(target_os = "linux")]
pub(crate) fn read_private_connection(path: &Path) -> Result<Connection, PrivateSqliteError> {
    let directory = path
        .parent()
        .ok_or_else(|| PrivateSqliteError::UnsafePath {
            path: path.into(),
            reason: "missing private state directory",
        })?;
    let file_name = path
        .file_name()
        .ok_or_else(|| PrivateSqliteError::UnsafePath {
            path: path.into(),
            reason: "missing private database file name",
        })?;
    // Resolve only the caller's storage root; never canonicalize the protected
    // private subdirectory itself so SQLITE_OPEN_NOFOLLOW still rejects a
    // symlink swapped into place after symlink_metadata.
    let directory = resolve_private_directory(directory)?;
    let database = directory.join(file_name);
    // A query may share this process with a writer. Closing descriptors to
    // SQLite files can release process-wide POSIX locks, including WAL locks.
    let check = |entry: &Path, mode, directory| {
        let metadata = fs::symlink_metadata(entry)
            .map_err(|source| io_error("inspect private path", entry, source))?;
        let trusted = if directory {
            metadata.is_dir()
        } else {
            metadata.is_file()
        };
        // SAFETY: geteuid has no preconditions.
        if !trusted
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o7777 != mode
            || !directory && metadata.nlink() != 1
        {
            return Err(PrivateSqliteError::UnsafePath {
                path: entry.into(),
                reason: "private path has invalid type, owner, permissions, or links",
            });
        }
        Ok(())
    };
    check(&directory, 0o700, true)?;
    check(&database, 0o600, false)?;
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut sidecar = database.as_os_str().to_owned();
        sidecar.push(suffix);
        match check(Path::new(&sidecar), 0o600, false) {
            Err(PrivateSqliteError::Io { source, .. })
                if source.kind() == io::ErrorKind::NotFound => {}
            result => result?,
        }
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW;
    let connection = Connection::open_with_flags(&database, flags)?;
    connection.busy_timeout(Duration::from_millis(500))?;
    Ok(connection)
}

fn split_private_directory(
    directory: &Path,
) -> Result<(&Path, &std::ffi::OsStr), PrivateSqliteError> {
    let storage_root = match directory.parent() {
        Some(parent) if parent.as_os_str().is_empty() => Path::new("."),
        Some(parent) => parent,
        None => {
            return Err(PrivateSqliteError::UnsafePath {
                path: directory.into(),
                reason: "missing storage root for private state directory",
            });
        }
    };
    let dir_name = directory
        .file_name()
        .ok_or_else(|| PrivateSqliteError::UnsafePath {
            path: directory.into(),
            reason: "missing private state directory name",
        })?;
    Ok((storage_root, dir_name))
}

fn resolve_private_directory(directory: &Path) -> Result<PathBuf, PrivateSqliteError> {
    let (storage_root, dir_name) = split_private_directory(directory)?;
    let canonical_root = fs::canonicalize(storage_root)
        .map_err(|source| io_error("resolve storage root", storage_root, source))?;
    Ok(canonical_root.join(dir_name))
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

    #[cfg(target_os = "linux")]
    #[test]
    fn read_private_connection_supports_symlinked_storage_root_and_rejects_state_symlinks() {
        use super::{PrivateSqliteError, read_private_connection};

        let root = std::env::temp_dir().join(format!(
            "agentsight-private-read-symlink-{}",
            uuid::Uuid::new_v4()
        ));
        let real_base = root.join("real-storage");
        fs::create_dir_all(&real_base).expect("real storage root should exist");
        let alias_base = root.join("storage-alias");
        symlink(&real_base, &alias_base).expect("storage root alias should exist");

        let alias_state = alias_base.join("aw-audit");
        let writer = open_private_connection(&alias_state, "audit.db")
            .expect("writer should open through symlinked storage root");
        writer
            .execute_batch("CREATE TABLE fixture (value INTEGER); INSERT INTO fixture VALUES (42);")
            .expect("writer should persist row");

        let alias_db = alias_state.join("audit.db");
        let reader = read_private_connection(&alias_db)
            .expect("reader should open through the same symlinked storage root");
        let value: i64 = reader
            .query_row("SELECT value FROM fixture", [], |row| row.get(0))
            .expect("reader should query persisted row");
        assert_eq!(value, 42);
        drop(reader);
        drop(writer);

        // Symlinked sidecars (-wal / -shm / -journal) must be rejected.
        let decoy = root.join("decoy");
        fs::write(&decoy, []).expect("decoy file should exist");
        fs::set_permissions(&decoy, fs::Permissions::from_mode(0o600)).expect("decoy mode");
        let real_state = real_base.join("aw-audit");
        for suffix in ["-wal", "-shm", "-journal"] {
            let sidecar = real_state.join(format!("audit.db{suffix}"));
            let _ = fs::remove_file(&sidecar);
            symlink(&decoy, &sidecar).expect("sidecar symlink should exist");
            let err =
                read_private_connection(&alias_db).expect_err("symlinked sidecar must be rejected");
            assert!(
                matches!(err, PrivateSqliteError::UnsafePath { .. }),
                "expected UnsafePath for {suffix}, got {err}"
            );
            fs::remove_file(&sidecar).expect("sidecar symlink should be removed");
        }

        // Symlinked database file must be rejected.
        let real_db = real_state.join("audit.db");
        let moved_db = root.join("moved.db");
        fs::rename(&real_db, &moved_db).expect("database file should move");
        symlink(&moved_db, &real_db).expect("database symlink should exist");
        let err = read_private_connection(&alias_db)
            .expect_err("symlinked database file must be rejected");
        assert!(matches!(err, PrivateSqliteError::UnsafePath { .. }));
        fs::remove_file(&real_db).expect("database symlink should be removed");
        fs::rename(&moved_db, &real_db).expect("database file should restore");

        // Symlinked aw-audit directory itself must be rejected even when target is 0700.
        let moved_state = root.join("moved-state");
        fs::rename(&real_state, &moved_state).expect("state dir should move");
        symlink(&moved_state, &real_state).expect("state dir symlink should exist");
        let err = read_private_connection(&alias_db)
            .expect_err("symlinked private directory must be rejected");
        assert!(matches!(err, PrivateSqliteError::UnsafePath { .. }));

        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_private_directory_only_canonicalizes_storage_root_and_preserves_nofollow_on_swap() {
        use super::{PrivateSqliteError, read_private_connection, resolve_private_directory};
        use rusqlite::{Connection, OpenFlags};

        let root = std::env::temp_dir().join(format!(
            "agentsight-private-root-only-canon-{}",
            uuid::Uuid::new_v4()
        ));
        let real_base = root.join("real-storage");
        fs::create_dir_all(&real_base).expect("real storage root should exist");
        let alias_base = root.join("storage-alias");
        symlink(&real_base, &alias_base).expect("storage root alias should exist");

        let alias_state = alias_base.join("aw-audit");
        let writer = open_private_connection(&alias_state, "audit.db")
            .expect("writer should initialize real aw-audit under symlinked root");
        writer
            .execute_batch("CREATE TABLE fixture (value INTEGER); INSERT INTO fixture VALUES (7);")
            .expect("writer should persist row");
        drop(writer);

        let canonical_base = fs::canonicalize(&real_base).expect("canonical base");
        let resolved_before_swap =
            resolve_private_directory(&alias_state).expect("resolve should succeed");
        assert_eq!(resolved_before_swap, canonical_base.join("aw-audit"));

        // Simulate a TOCTOU directory swap after validation: replace aw-audit with a
        // symlink to a valid 0700 directory containing the valid 0600 audit.db.
        let real_state = real_base.join("aw-audit");
        let swapped_target = root.join("swapped-target");
        fs::rename(&real_state, &swapped_target).expect("move aw-audit");
        symlink(&swapped_target, &real_state).expect("replace aw-audit with symlink");

        // resolve_private_directory must still return <canonical_root>/aw-audit without
        // following the symlink at aw-audit itself.
        let resolved_after_swap =
            resolve_private_directory(&alias_state).expect("resolve should keep literal dir name");
        assert_eq!(resolved_after_swap, canonical_base.join("aw-audit"));
        assert!(
            fs::symlink_metadata(&resolved_after_swap)
                .expect("symlink metadata")
                .file_type()
                .is_symlink(),
            "protected aw-audit directory name must remain un-canonicalized"
        );

        // Because the literal aw-audit component is preserved, SQLite NOFOLLOW still
        // rejects opening the database even if the swap happened after metadata checks.
        let nofollow_err = Connection::open_with_flags(
            resolved_before_swap.join("audit.db"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
        )
        .expect_err("SQLITE_OPEN_NOFOLLOW must reject a swapped aw-audit symlink");
        assert!(
            !nofollow_err.to_string().is_empty(),
            "expected SQLite error on swapped directory symlink"
        );

        // Both read and write entrypoints reject the swapped aw-audit symlink.
        let read_err = read_private_connection(&alias_state.join("audit.db"))
            .expect_err("read_private_connection must reject swapped aw-audit symlink");
        assert!(matches!(read_err, PrivateSqliteError::UnsafePath { .. }));
        assert!(
            open_private_connection(&alias_state, "audit.db").is_err(),
            "open_private_connection must reject swapped aw-audit symlink"
        );

        fs::remove_dir_all(root).expect("fixture directory should be removed");
    }
}
