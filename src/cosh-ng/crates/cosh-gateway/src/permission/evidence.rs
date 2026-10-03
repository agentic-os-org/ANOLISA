//! Private append-only storage for redacted permission evidence.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use nix::fcntl::{Flock, FlockArg};
use thiserror::Error;

use super::{PermissionEvidence, PermissionEvidenceSink};

const MAX_EVIDENCE_BYTES: usize = 16 * 1024;

/// Safe durable-evidence failure without record contents.
#[derive(Debug, Error)]
pub enum PermissionEvidenceError {
    /// Evidence paths must be explicit and private.
    #[error("permission evidence path is unsafe")]
    UnsafePath,
    /// Evidence encoding exceeded its bounded schema.
    #[error("permission evidence record exceeds its size bound")]
    RecordTooLarge,
    /// Filesystem persistence failed.
    #[error("permission evidence persistence failed: {0}")]
    Io(#[from] std::io::Error),
    /// Evidence serialization failed.
    #[error("permission evidence serialization failed: {0}")]
    Serialize(#[from] serde_json::Error),
}

/// Process-owned writer for one private append-only JSONL evidence file.
#[derive(Debug)]
pub struct FilePermissionEvidenceSink {
    path: PathBuf,
    writer: BufWriter<LockedFile>,
}

#[derive(Debug)]
struct LockedFile(Flock<File>);

impl Write for LockedFile {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        Write::write(&mut *self.0, bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Write::flush(&mut *self.0)
    }
}

impl LockedFile {
    fn sync_data(&self) -> std::io::Result<()> {
        self.0.sync_data()
    }
}

impl FilePermissionEvidenceSink {
    /// Opens a private evidence file below an existing private directory.
    ///
    /// # Errors
    ///
    /// Rejects relative paths, symlinks, unsafe ownership/mode, and lock errors.
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, PermissionEvidenceError> {
        let path = path.into();
        validate_path(&path)?;
        let parent = path.parent().ok_or(PermissionEvidenceError::UnsafePath)?;
        validate_private_directory(parent)?;
        let mut options = OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
        }
        let file = options.open(&path)?;
        validate_private_file(&file)?;
        let locked = Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| std::io::Error::from_raw_os_error(error as i32))?;
        Ok(Self {
            path,
            writer: BufWriter::new(LockedFile(locked)),
        })
    }

    /// Creates the final private state directory when absent, then opens evidence.
    ///
    /// # Errors
    ///
    /// Rejects non-absolute paths, symlinked state directories, unsafe ownership
    /// or modes, and filesystem persistence failures.
    #[cfg(unix)]
    pub fn open_in_private_state(
        path: impl Into<PathBuf>,
    ) -> Result<Self, PermissionEvidenceError> {
        use std::os::unix::fs::PermissionsExt;

        let path = path.into();
        validate_path(&path)?;
        let parent = path.parent().ok_or(PermissionEvidenceError::UnsafePath)?;
        create_private_directory_chain(parent)?;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
        validate_private_directory(parent)?;
        Self::open(path)
    }

    /// Returns the configured basename without exposing the directory.
    pub fn basename(&self) -> Option<&str> {
        self.path.file_name().and_then(|name| name.to_str())
    }
}

impl PermissionEvidenceSink for FilePermissionEvidenceSink {
    fn record(&mut self, evidence: &PermissionEvidence) -> Result<(), PermissionEvidenceError> {
        let mut bytes = serde_json::to_vec(evidence)?;
        bytes.push(b'\n');
        if bytes.len() > MAX_EVIDENCE_BYTES {
            return Err(PermissionEvidenceError::RecordTooLarge);
        }
        self.writer.write_all(&bytes)?;
        self.writer.flush()?;
        self.writer.get_ref().sync_data()?;
        Ok(())
    }
}

fn validate_path(path: &Path) -> Result<(), PermissionEvidenceError> {
    if !path.is_absolute()
        || path.file_name().is_none()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir | std::path::Component::CurDir
            )
        })
    {
        return Err(PermissionEvidenceError::UnsafePath);
    }
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err(PermissionEvidenceError::UnsafePath);
    }
    Ok(())
}

fn validate_private_directory(path: &Path) -> Result<(), PermissionEvidenceError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(PermissionEvidenceError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != nix::unistd::Uid::effective().as_raw() || metadata.mode() & 0o077 != 0
        {
            return Err(PermissionEvidenceError::UnsafePath);
        }
    }
    Ok(())
}

#[cfg(unix)]
fn validate_owned_directory(path: &Path) -> Result<(), PermissionEvidenceError> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir()
        || metadata.file_type().is_symlink()
        || metadata.uid() != nix::unistd::Uid::effective().as_raw()
        || metadata.mode() & 0o022 != 0
    {
        return Err(PermissionEvidenceError::UnsafePath);
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_directory_chain(path: &Path) -> Result<(), PermissionEvidenceError> {
    use std::os::unix::fs::DirBuilderExt;

    let mut missing = Vec::new();
    let mut cursor = path;
    loop {
        match fs::symlink_metadata(cursor) {
            Ok(_) => {
                validate_owned_directory(cursor)?;
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                missing.push(cursor.to_path_buf());
                cursor = cursor.parent().ok_or(PermissionEvidenceError::UnsafePath)?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    for directory in missing.into_iter().rev() {
        let mut builder = fs::DirBuilder::new();
        builder.mode(0o700).create(&directory)?;
        validate_private_directory(&directory)?;
    }
    Ok(())
}

fn validate_private_file(file: &File) -> Result<(), PermissionEvidenceError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(PermissionEvidenceError::UnsafePath);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != nix::unistd::Uid::effective().as_raw() || metadata.mode() & 0o077 != 0
        {
            return Err(PermissionEvidenceError::UnsafePath);
        }
    }
    Ok(())
}
