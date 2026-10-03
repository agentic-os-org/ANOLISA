use thiserror::Error;

#[derive(Debug, Error)]
pub enum MemoryError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("path '{0}' is outside the namespace mount point")]
    PathOutsideMount(String),

    #[error(
        "path '{0}' targets a reserved segment (.anolisa, .git, .gitignore) and is not writable by tools"
    )]
    TargetIsReserved(String),

    #[error("file not found: {0}")]
    NotFound(String),

    #[error("file already exists: {0}")]
    AlreadyExists(String),

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("not implemented: {0}")]
    NotImplemented(&'static str),

    #[error("regex: {0}")]
    Regex(#[from] regex::Error),

    #[error("glob: {0}")]
    Glob(#[from] globset::Error),

    #[error("serde_json: {0}")]
    Json(#[from] serde_json::Error),

    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("git: {0}")]
    Git(#[from] git2::Error),

    #[error("nix: {0}")]
    Nix(#[from] nix::Error),

    /// `unshare(NEWUSER|NEWNS)` succeeded but a follow-up step in the
    /// same atomic stage (setgroups / uid_map / gid_map) failed. The
    /// process is now inside a half-initialised user namespace where
    /// it appears as `nobody/nogroup`, so silently falling back to a
    /// userland mount is unsafe — every subsequent home-dir syscall
    /// would behave unexpectedly. `auto` fallback must propagate this
    /// instead of swallowing it.
    #[error("user namespace half-initialised, cannot recover: {0}")]
    UserNsUnrecoverable(String),

    #[error("other: {0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, MemoryError>;
