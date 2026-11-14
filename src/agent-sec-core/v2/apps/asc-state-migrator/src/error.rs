//! Failures of the state migrator, mapped to exit code 1.

/// Everything that aborts a migrator command with a diagnostic.
#[derive(Debug, thiserror::Error)]
pub enum MigratorError {
    /// A command-line value cannot be used as given.
    #[error("usage: {0}")]
    Usage(String),

    /// An explicitly requested source directory cannot be used.
    #[error("source '{path}' is not usable: {reason}")]
    SourceUnusable {
        /// The rejected directory.
        path: String,
        /// Why the directory was rejected.
        reason: String,
    },

    /// The destination database cannot be used.
    #[error("destination '{path}' is not usable: {reason}")]
    DestinationUnusable {
        /// The destination path.
        path: String,
        /// What went wrong.
        reason: String,
    },

    /// No usable source remained after discovery and validation.
    #[error("no usable source to migrate")]
    NoSources,

    /// The destination store could not be opened or converged.
    #[error("destination store: {0}")]
    Kernel(#[from] asc_sqlite_kernel::KernelError),

    /// A `SQLite` operation failed.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// Filesystem access failed.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    /// The run journal could not be read or updated.
    #[error("journal '{path}': {reason}")]
    Journal {
        /// The journal path.
        path: String,
        /// What went wrong.
        reason: String,
    },

    /// A requested run id is not in the journal.
    #[error("run '{0}' not found in the journal")]
    RunNotFound(String),

    /// A run was already rolled back and cannot be rolled back again.
    #[error("run '{0}' was already rolled back")]
    AlreadyRolledBack(String),

    /// A source was modified too recently to assume its writers are stopped.
    #[error("source '{path}' was modified {age_seconds}s ago; stop all v1 writers or pass --force")]
    SourceRecentlyWritten {
        /// The recently written path.
        path: String,
        /// Age of the newest modification, in seconds.
        age_seconds: u64,
    },

    /// A timestamp in a source record could not be normalized.
    #[error("timestamp: {0}")]
    Timestamp(#[from] asc_security_events::TimestampError),

    /// The default destination could not be resolved.
    #[error("destination resolution: {0}")]
    Config(#[from] asc_security_events::ConfigError),

    /// A `JSONL` recovery stream could not be opened.
    #[error("jsonl recovery: {0}")]
    Jsonl(String),
}
