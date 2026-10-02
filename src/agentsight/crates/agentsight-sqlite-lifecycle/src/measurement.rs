use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::LifecycleError;

/// Current physical and reusable capacity of one SQLite database.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeSnapshot {
    /// Bytes allocated by the main database file.
    pub database_bytes: u64,
    /// Bytes allocated by the WAL sidecar.
    pub wal_bytes: u64,
    /// Bytes allocated by the shared-memory sidecar.
    pub shm_bytes: u64,
    /// Main-file bytes currently present on SQLite's freelist.
    pub freelist_bytes: u64,
    /// Main, WAL, and SHM bytes combined.
    pub physical_bytes: u64,
    /// Physical bytes minus reusable freelist pages.
    pub logical_bytes: u64,
}

/// Measures a database without checkpointing or mutating it.
///
/// # Errors
///
/// Returns an error when the main file cannot be inspected or SQLite cannot
/// report its page and freelist counts.
pub fn measure_database(
    path: &Path,
    connection: &Connection,
) -> Result<SizeSnapshot, LifecycleError> {
    let in_memory = path == Path::new(":memory:");
    let database_bytes = if in_memory {
        0
    } else {
        file_size(path, false)?
    };
    let wal_bytes = if in_memory {
        0
    } else {
        file_size(&sidecar_path(path, "-wal"), true)?
    };
    let shm_bytes = if in_memory {
        0
    } else {
        file_size(&sidecar_path(path, "-shm"), true)?
    };
    let physical_bytes = database_bytes
        .saturating_add(wal_bytes)
        .saturating_add(shm_bytes);

    let freelist_pages: u64 =
        connection.query_row("PRAGMA freelist_count", [], |row| row.get(0))?;
    let page_size: u64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let freelist_bytes = freelist_pages.saturating_mul(page_size);

    Ok(SizeSnapshot {
        database_bytes,
        wal_bytes,
        shm_bytes,
        freelist_bytes,
        physical_bytes,
        logical_bytes: physical_bytes.saturating_sub(freelist_bytes),
    })
}

fn file_size(path: &Path, missing_is_zero: bool) -> Result<u64, LifecycleError> {
    match std::fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(source) if missing_is_zero && source.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Err(LifecycleError::DatabaseMissing(path.to_path_buf()))
        }
        Err(source) => Err(LifecycleError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let mut sidecar = path.as_os_str().to_os_string();
    sidecar.push(suffix);
    PathBuf::from(sidecar)
}

#[cfg(test)]
mod tests {
    use std::{fs, time::SystemTime};

    use crate::{ConnectionOptions, open_connection};

    use super::*;

    fn test_path() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system time must follow Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "agentsight-measurement-{}-{nonce}.db",
            std::process::id()
        ))
    }

    #[test]
    fn measurement_includes_sidecars_and_freelist() {
        let path = test_path();
        let connection = open_connection(&path, ConnectionOptions::default()).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE records (payload TEXT NOT NULL);\
                 INSERT INTO records(payload) VALUES (zeroblob(65536));\
                 DELETE FROM records;",
            )
            .unwrap();

        let snapshot = measure_database(&path, &connection).unwrap();
        assert!(snapshot.database_bytes > 0);
        assert_eq!(
            snapshot.physical_bytes,
            snapshot.database_bytes + snapshot.wal_bytes + snapshot.shm_bytes
        );
        assert!(snapshot.logical_bytes <= snapshot.physical_bytes);

        drop(connection);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(sidecar_path(&path, "-wal"));
        let _ = fs::remove_file(sidecar_path(&path, "-shm"));
    }
}
