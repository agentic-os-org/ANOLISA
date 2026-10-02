//! Managed extension store paths, source metadata, locking, and safe local materialization.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{user_extensions_dir, MANAGED_INSTALL_METADATA_FILENAME};

/// Directory containing managed installations below the extension root.
pub const MANAGED_DIR: &str = ".managed";
/// Directory containing prepared, uncommitted installations.
pub const STAGING_DIR: &str = ".staging";
/// Directory containing the current transaction rollback candidate.
pub const ROLLBACK_DIR: &str = ".rollback";
/// Directory containing operation records and receipts.
pub const OPERATIONS_DIR: &str = ".operations";
/// Advisory lock file for all managed store mutations.
pub const STORE_LOCK_FILE: &str = ".store.lock";
/// Managed package payload entry.
pub const PAYLOAD_DIR: &str = "payload";

/// Source kinds supported by managed installation metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ManagedSourceKind {
    /// Local directory copied into the managed store.
    PathCopy,
    /// Development directory linked from the managed store.
    Link,
    /// HTTPS Git source pinned to a resolved commit.
    GitHttps,
}

/// Versioned installation metadata stored outside the package payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ManagedInstallationMetadata {
    /// Metadata schema version.
    pub schema_version: u32,
    /// Validated package identity.
    pub name: String,
    /// Installed package version.
    pub version: String,
    /// Materialized source kind.
    pub source_kind: ManagedSourceKind,
    /// Canonical local path or final HTTPS remote identity.
    pub source_identity: String,
    /// Requested Git ref, if applicable.
    pub requested_ref: Option<String>,
    /// Resolved Git commit, if applicable.
    pub resolved_revision: Option<String>,
    /// Deterministic package content digest.
    pub content_digest: String,
    /// Capability security fingerprint accepted by commit.
    pub capability_fingerprint: String,
    /// Operation/consent record that authorized installation.
    pub consent_reference: String,
    /// Consent policy version applied to the referenced record.
    #[serde(default)]
    pub consent_policy_version: u32,
    /// Initial installation time.
    pub installed_at: DateTime<Utc>,
    /// Last successful update time.
    pub updated_at: DateTime<Utc>,
}

/// Canonical paths owned by the managed extension store.
#[derive(Debug, Clone)]
pub struct StorePaths {
    /// Existing user extension root.
    pub root: PathBuf,
}

impl StorePaths {
    /// Resolves the production user extension root.
    pub fn for_current_user() -> Result<Self, SourceError> {
        let root = user_extensions_dir().ok_or_else(|| {
            SourceError::new(
                "extension_store_unavailable",
                "cannot determine user extension directory",
            )
        })?;
        Ok(Self { root })
    }

    /// Creates an isolated store path for tests and internal callers.
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Returns the managed installation directory for a validated package name.
    pub fn installation(&self, name: &str) -> PathBuf {
        self.root.join(MANAGED_DIR).join(name)
    }

    /// Returns the prepared transaction directory.
    pub fn staging(&self, operation_id: &str) -> PathBuf {
        self.root.join(STAGING_DIR).join(operation_id)
    }

    /// Returns the rollback candidate directory.
    pub fn rollback(&self, operation_id: &str) -> PathBuf {
        self.root.join(ROLLBACK_DIR).join(operation_id)
    }

    /// Returns the journal controlling recovery of an uninstall rollback directory.
    pub fn rollback_journal(&self, operation_id: &str) -> PathBuf {
        self.root
            .join(ROLLBACK_DIR)
            .join(format!("{operation_id}.uninstall.json"))
    }

    /// Returns the journal keeping a published package provisional until health validation.
    pub fn pending_commit_journal(&self, operation_id: &str) -> PathBuf {
        self.root
            .join(ROLLBACK_DIR)
            .join(format!("{operation_id}.commit.json"))
    }

    /// Returns the journal for one desired-state or source-selection mutation.
    pub fn pending_state_journal(&self, operation_id: &str) -> PathBuf {
        self.root
            .join(ROLLBACK_DIR)
            .join(format!("{operation_id}.state.json"))
    }

    /// Returns the operation record path.
    pub fn operation(&self, operation_id: &str) -> PathBuf {
        self.root
            .join(OPERATIONS_DIR)
            .join(format!("{operation_id}.json"))
    }

    /// Returns the completed operation receipt path.
    pub fn receipt(&self, operation_id: &str) -> PathBuf {
        self.root
            .join(OPERATIONS_DIR)
            .join(format!("{operation_id}.result.json"))
    }

    /// Creates internal store directories without touching package contents.
    pub fn ensure_internal_dirs(&self) -> Result<(), SourceError> {
        for directory in [MANAGED_DIR, STAGING_DIR, ROLLBACK_DIR, OPERATIONS_DIR] {
            fs::create_dir_all(self.root.join(directory)).map_err(|error| {
                SourceError::new(
                    "extension_store_write_failed",
                    format!(
                        "failed to create extension store directory {}: {error}",
                        self.root.join(directory).display()
                    ),
                )
            })?;
        }
        Ok(())
    }

    /// Acquires the advisory mutation lock before the timeout elapses.
    pub fn lock(&self, timeout: Duration) -> Result<StoreLock, SourceError> {
        fs::create_dir_all(&self.root).map_err(|error| {
            SourceError::new(
                "extension_store_write_failed",
                format!("failed to create {}: {error}", self.root.display()),
            )
        })?;
        let path = self.root.join(STORE_LOCK_FILE);
        let file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&path)
            .map_err(|error| {
                SourceError::new(
                    "extension_store_lock_failed",
                    format!("failed to open {}: {error}", path.display()),
                )
            })?;
        let started = Instant::now();
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => return Ok(StoreLock { file }),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if started.elapsed() >= timeout {
                        return Err(SourceError::new(
                            "extension_store_lock_timeout",
                            format!("timed out waiting for {}", path.display()),
                        ));
                    }
                    thread::sleep(Duration::from_millis(20));
                }
                Err(error) => {
                    return Err(SourceError::new(
                        "extension_store_lock_failed",
                        format!("failed to lock {}: {error}", path.display()),
                    ));
                }
            }
        }
    }
}

/// Held advisory store lock.
#[derive(Debug)]
pub struct StoreLock {
    file: File,
}

impl Drop for StoreLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

/// Stable source/store failure.
#[derive(Debug)]
pub struct SourceError {
    code: &'static str,
    message: String,
}

impl SourceError {
    /// Builds a stable source error.
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable diagnostic code.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for SourceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SourceError {}

/// Canonicalizes and validates a local extension source directory.
pub fn canonical_local_source(source: &Path) -> Result<PathBuf, SourceError> {
    let canonical = source.canonicalize().map_err(|error| {
        SourceError::new(
            "extension_source_unreadable",
            format!("failed to canonicalize {}: {error}", source.display()),
        )
    })?;
    if !canonical.is_dir() {
        return Err(SourceError::new(
            "extension_source_not_directory",
            format!(
                "extension source is not a directory: {}",
                canonical.display()
            ),
        ));
    }
    Ok(canonical)
}

/// Copies a local package without following symlinks or accepting special files.
pub fn copy_package_tree(source: &Path, destination: &Path) -> Result<(), SourceError> {
    fs::create_dir_all(destination).map_err(|error| {
        SourceError::new(
            "extension_staging_failed",
            format!("failed to create {}: {error}", destination.display()),
        )
    })?;
    copy_directory_contents(source, destination, source)
}

fn copy_directory_contents(
    source: &Path,
    destination: &Path,
    root: &Path,
) -> Result<(), SourceError> {
    let entries = fs::read_dir(source).map_err(|error| {
        SourceError::new(
            "extension_source_unreadable",
            format!("failed to read {}: {error}", source.display()),
        )
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            SourceError::new(
                "extension_source_unreadable",
                format!("failed to read entry below {}: {error}", source.display()),
            )
        })?;
        if entry.file_name() == ".git" {
            continue;
        }
        let source_path = entry.path();
        let relative = source_path.strip_prefix(root).map_err(|error| {
            SourceError::new(
                "extension_path_escape",
                format!("source path escaped package root: {error}"),
            )
        })?;
        validate_relative_path(relative)?;
        let destination_path = destination.join(relative);
        let file_type = entry.file_type().map_err(|error| {
            SourceError::new(
                "extension_source_unreadable",
                format!("failed to inspect {}: {error}", source_path.display()),
            )
        })?;
        if file_type.is_symlink() {
            return Err(SourceError::new(
                "extension_source_symlink_unsupported",
                format!(
                    "path-copy packages cannot contain symlinks: {}",
                    source_path.display()
                ),
            ));
        }
        if file_type.is_dir() {
            fs::create_dir_all(&destination_path).map_err(|error| {
                SourceError::new(
                    "extension_staging_failed",
                    format!("failed to create {}: {error}", destination_path.display()),
                )
            })?;
            copy_directory_contents(&source_path, destination, root)?;
        } else if file_type.is_file() {
            if let Some(parent) = destination_path.parent() {
                fs::create_dir_all(parent).map_err(|error| {
                    SourceError::new(
                        "extension_staging_failed",
                        format!("failed to create {}: {error}", parent.display()),
                    )
                })?;
            }
            fs::copy(&source_path, &destination_path).map_err(|error| {
                SourceError::new(
                    "extension_staging_failed",
                    format!(
                        "failed to copy {} to {}: {error}",
                        source_path.display(),
                        destination_path.display()
                    ),
                )
            })?;
        } else {
            return Err(SourceError::new(
                "extension_source_special_file",
                format!("unsupported special file: {}", source_path.display()),
            ));
        }
    }
    Ok(())
}

/// Computes a deterministic digest over package-relative paths and file bytes.
pub fn content_digest(root: &Path) -> Result<String, SourceError> {
    let mut files = Vec::new();
    collect_files(root, root, &mut files)?;
    files.sort();
    let mut hasher = Sha256::new();
    for relative in files {
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        let path = root.join(&relative);
        let mut file = File::open(&path).map_err(|error| {
            SourceError::new(
                "extension_source_unreadable",
                format!("failed to open {}: {error}", path.display()),
            )
        })?;
        let mut buffer = [0_u8; 8192];
        loop {
            let read = file.read(&mut buffer).map_err(|error| {
                SourceError::new(
                    "extension_source_unreadable",
                    format!("failed to read {}: {error}", path.display()),
                )
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        hasher.update([0xff]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), SourceError> {
    for entry in fs::read_dir(directory).map_err(|error| {
        SourceError::new(
            "extension_source_unreadable",
            format!("failed to read {}: {error}", directory.display()),
        )
    })? {
        let entry = entry.map_err(|error| {
            SourceError::new(
                "extension_source_unreadable",
                format!("failed to read package entry: {error}"),
            )
        })?;
        if entry.file_name() == ".git" {
            continue;
        }
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|error| {
            SourceError::new(
                "extension_path_escape",
                format!("package path escaped root: {error}"),
            )
        })?;
        validate_relative_path(relative)?;
        let file_type = entry.file_type().map_err(|error| {
            SourceError::new(
                "extension_source_unreadable",
                format!("failed to inspect {}: {error}", path.display()),
            )
        })?;
        if file_type.is_symlink() {
            return Err(SourceError::new(
                "extension_source_symlink_unsupported",
                format!("package digest refuses symlink: {}", path.display()),
            ));
        }
        if file_type.is_dir() {
            collect_files(root, &path, files)?;
        } else if file_type.is_file() {
            files.push(relative.to_path_buf());
        } else {
            return Err(SourceError::new(
                "extension_source_special_file",
                format!("package digest refuses special file: {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn validate_relative_path(path: &Path) -> Result<(), SourceError> {
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(SourceError::new(
            "extension_path_escape",
            format!("invalid package-relative path: {}", path.display()),
        ));
    }
    Ok(())
}

/// Writes managed metadata atomically within an installation directory.
pub fn write_metadata(
    installation: &Path,
    metadata: &ManagedInstallationMetadata,
) -> Result<(), SourceError> {
    let bytes = serde_json::to_vec_pretty(metadata).map_err(|error| {
        SourceError::new(
            "extension_metadata_write_failed",
            format!("failed to serialize installation metadata: {error}"),
        )
    })?;
    let target = installation.join(MANAGED_INSTALL_METADATA_FILENAME);
    let temporary = installation.join(format!(".{MANAGED_INSTALL_METADATA_FILENAME}.tmp"));
    fs::write(&temporary, bytes).map_err(|error| {
        SourceError::new(
            "extension_metadata_write_failed",
            format!("failed to write {}: {error}", temporary.display()),
        )
    })?;
    fs::rename(&temporary, &target).map_err(|error| {
        SourceError::new(
            "extension_metadata_write_failed",
            format!("failed to replace {}: {error}", target.display()),
        )
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_copy_rejects_symlink() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        std::fs::write(source.path().join("real"), "data").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(source.path().join("real"), source.path().join("link")).unwrap();
        #[cfg(unix)]
        assert_eq!(
            copy_package_tree(source.path(), &destination.path().join("payload"))
                .unwrap_err()
                .code(),
            "extension_source_symlink_unsupported"
        );
    }

    #[test]
    fn content_digest_is_stable_across_creation_order() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(first.path().join("a"), "one").unwrap();
        std::fs::write(first.path().join("b"), "two").unwrap();
        std::fs::write(second.path().join("b"), "two").unwrap();
        std::fs::write(second.path().join("a"), "one").unwrap();
        assert_eq!(
            content_digest(first.path()).unwrap(),
            content_digest(second.path()).unwrap()
        );
    }

    #[test]
    fn store_lock_times_out_under_contention() {
        let root = tempfile::tempdir().unwrap();
        let paths = StorePaths::new(root.path().join("extensions"));
        let _first = paths.lock(Duration::from_millis(50)).unwrap();
        let error = paths.lock(Duration::from_millis(40)).unwrap_err();
        assert_eq!(error.code(), "extension_store_lock_timeout");
    }
}
