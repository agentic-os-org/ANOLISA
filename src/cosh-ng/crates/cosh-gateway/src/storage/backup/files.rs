fn online_copy(source: &Connection, destination: &mut Connection) -> Result<(), StoreError> {
    let backup = Backup::new(source, destination)?;
    let mut transient_failures = 0_u32;
    loop {
        match backup.step(BACKUP_PAGES_PER_STEP)? {
            StepResult::Done => return Ok(()),
            StepResult::More => thread::sleep(BACKUP_RETRY_PAUSE),
            state @ (StepResult::Busy | StepResult::Locked) => {
                transient_failures = transient_failures.saturating_add(1);
                if transient_failures >= BACKUP_RETRY_LIMIT {
                    let error_code = if matches!(state, StepResult::Locked) {
                        rusqlite::ffi::SQLITE_LOCKED
                    } else {
                        rusqlite::ffi::SQLITE_BUSY
                    };
                    return Err(StoreError::Sqlite(rusqlite::Error::SqliteFailure(
                        rusqlite::ffi::Error::new(error_code),
                        Some("online backup remained busy".to_owned()),
                    )));
                }
                thread::sleep(BACKUP_RETRY_PAUSE);
            }
            _ => {
                return Err(StoreError::Corrupt {
                    message: "SQLite returned an unsupported online backup state".to_owned(),
                });
            }
        }
    }
}

fn verify_backup_path(
    path: &Path,
    expected_installation_id: &InstallationId,
) -> Result<(), StoreError> {
    sqlite::validate_existing_private_file_path(path)?;
    require_self_contained_database(path)?;
    let connection = open_read_only_database(path)?;
    verify_connection(&connection, expected_installation_id)
}

fn verify_connection(
    connection: &Connection,
    expected_installation_id: &InstallationId,
) -> Result<(), StoreError> {
    schema::preflight_existing(connection)?;
    let stored = read_installation_id(connection)?.ok_or_else(|| StoreError::Corrupt {
        message: "backup has no recoverable installation identity".to_owned(),
    })?;
    if &stored != expected_installation_id {
        return Err(StoreError::LedgerConflict {
            message: "backup belongs to another installation identity".to_owned(),
        });
    }
    Ok(())
}

fn read_installation_id(connection: &Connection) -> Result<Option<InstallationId>, StoreError> {
    let has_identity_table = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM sqlite_schema
             WHERE type = 'table' AND name = 'gateway_identity'
         )",
        [],
        |row| row.get::<_, bool>(0),
    )?;
    let stored = if has_identity_table {
        connection
            .query_row(
                "SELECT installation_id FROM gateway_identity WHERE singleton = 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(InstallationId::parse)
            .transpose()
            .map_err(|error| StoreError::Corrupt {
                message: format!("stored backup installation identity is invalid: {error}"),
            })?
    } else {
        None
    };
    if stored.is_some() {
        return Ok(stored);
    }
    sqlite::recover_installation_id(connection)
}

fn bind_restored_installation(
    connection: &Connection,
    expected_installation_id: &InstallationId,
) -> Result<(), StoreError> {
    connection.execute(
        "INSERT INTO gateway_identity(singleton, installation_id)
         VALUES (1, ?1)
         ON CONFLICT(singleton) DO NOTHING",
        params![expected_installation_id.as_str()],
    )?;
    Ok(())
}

fn open_read_only_database(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    sqlite::configure_read_only(&connection)?;
    Ok(connection)
}

fn open_temporary_database(path: &Path) -> Result<Connection, StoreError> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    configure_standalone_database(&connection)?;
    Ok(connection)
}

fn configure_standalone_database(connection: &Connection) -> Result<(), StoreError> {
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = DELETE;
         PRAGMA synchronous = FULL;
         PRAGMA busy_timeout = 5000;
         PRAGMA trusted_schema = OFF;",
    )?;
    Ok(())
}

struct TemporaryDatabase {
    path: PathBuf,
    file: File,
    cleanup: bool,
}

impl TemporaryDatabase {
    fn create(destination: &Path) -> Result<Self, StoreError> {
        let parent = destination.parent().ok_or_else(|| StoreError::UnsafePath {
            path: destination.to_path_buf(),
            message: "backup destination has no parent directory".to_owned(),
        })?;
        let file_name = destination
            .file_name()
            .ok_or_else(|| StoreError::UnsafePath {
                path: destination.to_path_buf(),
                message: "backup destination has no file name".to_owned(),
            })?;

        for _ in 0..TEMPORARY_CREATE_ATTEMPTS {
            let sequence = TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
            let mut temporary_name = OsString::from(".");
            temporary_name.push(file_name);
            temporary_name.push(format!(".{}.{}.tmp", std::process::id(), sequence));
            let path = parent.join(temporary_name);
            match create_private_file(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        cleanup: true,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => {
                    return Err(StoreError::Io {
                        operation: "create backup temporary file",
                        path,
                        source,
                    });
                }
            }
        }
        Err(StoreError::UnsafePath {
            path: destination.to_path_buf(),
            message: "could not allocate a unique backup temporary file".to_owned(),
        })
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn publish(mut self, destination: &Path) -> Result<(), StoreError> {
        self.file.sync_all().map_err(|source| StoreError::Io {
            operation: "sync verified backup file",
            path: self.path().to_path_buf(),
            source,
        })?;
        let temporary_path = self.path().to_path_buf();
        fs::hard_link(&temporary_path, destination).map_err(|source| StoreError::Io {
            operation: "atomically publish verified backup",
            path: destination.to_path_buf(),
            source,
        })?;
        fs::remove_file(&temporary_path).map_err(|source| StoreError::Io {
            operation: "remove published backup temporary name",
            path: temporary_path.clone(),
            source,
        })?;
        self.cleanup = false;
        sync_parent_directory(destination)
    }
}

impl Drop for TemporaryDatabase {
    fn drop(&mut self) {
        if self.cleanup {
            remove_sqlite_files(&self.path);
        }
    }
}

fn create_private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
        options.custom_flags(nix::libc::O_CLOEXEC | nix::libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn sync_parent_directory(path: &Path) -> Result<(), StoreError> {
    let parent = path.parent().ok_or_else(|| StoreError::UnsafePath {
        path: path.to_path_buf(),
        message: "storage path has no parent directory".to_owned(),
    })?;
    let directory = File::open(parent).map_err(|source| StoreError::Io {
        operation: "open storage parent directory for sync",
        path: parent.to_path_buf(),
        source,
    })?;
    directory.sync_all().map_err(|source| StoreError::Io {
        operation: "sync storage parent directory",
        path: parent.to_path_buf(),
        source,
    })
}

fn remove_sqlite_files(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut companion = path.as_os_str().to_owned();
        companion.push(suffix);
        let _ = fs::remove_file(PathBuf::from(companion));
    }
}

fn require_self_contained_database(path: &Path) -> Result<(), StoreError> {
    for suffix in ["-journal", "-wal", "-shm"] {
        let mut companion = path.as_os_str().to_owned();
        companion.push(suffix);
        let companion = PathBuf::from(companion);
        match fs::symlink_metadata(&companion) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Ok(_) => {
                return Err(StoreError::Corrupt {
                    message: format!(
                        "backup is not self-contained: unexpected SQLite {suffix} companion"
                    ),
                });
            }
            Err(source) => {
                return Err(StoreError::Io {
                    operation: "inspect backup companion file",
                    path: companion,
                    source,
                });
            }
        }
    }
    Ok(())
}
