//! Private single-writer segment storage for unified audit events.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use cosh_types::audit::{AuditComponentName, AuditEventV1, MAX_AUDIT_RECORD_BYTES};
use cosh_types::error::{CoshError, ErrorCode};
use nix::fcntl::{Flock, FlockArg};
use uuid::Uuid;

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

/// Fixed maximum size of one active segment before rotation.
pub const MAX_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;

/// Durability class for one semantic audit boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuditDurability {
    /// Flush after a bounded record count or elapsed interval.
    Ordinary,
    /// Call `sync_data` before reporting success.
    SecurityBoundary,
}

/// Process-owned writer for one active segment at a time.
pub struct AuditSegmentWriter {
    root: PathBuf,
    component: AuditComponentName,
    active: Option<ActiveSegment>,
}

struct ActiveSegment {
    date: String,
    sequence: u64,
    bytes: u64,
    active_path: PathBuf,
    file: BufWriter<LockedFile>,
}

struct LockedFile(Flock<File>);

impl Write for LockedFile {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        Write::write(&mut *self.0, buffer)
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

impl AuditSegmentWriter {
    /// Creates a writer below an already resolved audit root.
    ///
    /// # Errors
    ///
    /// Returns a safe audit error when the root, directory permissions,
    /// segment create-new open, or advisory lock cannot be established.
    pub fn create(root: &Path, component: AuditComponentName) -> Result<Self, CoshError> {
        validate_root_path(root)?;
        ensure_private_dir(root)?;
        ensure_private_dir(&root.join("v1"))?;
        ensure_private_dir(&root.join("v1/segments"))?;
        Ok(Self {
            root: root.to_path_buf(),
            component,
            active: None,
        })
    }

    /// Appends one validated event and applies the requested durability class.
    ///
    /// # Errors
    ///
    /// Rejects invalid or oversized events before writing bytes and returns
    /// safe operation/basename context for I/O failures.
    pub fn append(
        &mut self,
        event: &mut AuditEventV1,
        durability: AuditDurability,
    ) -> Result<(), CoshError> {
        event
            .validate()
            .map_err(|error| audit_error("validate record", None, error))?;
        self.ensure_active(Utc::now())?;

        let now = Utc::now();
        let sequence = self
            .active
            .as_ref()
            .map(|active| active.sequence)
            .unwrap_or(0);
        event.assign_writer_fields(sequence, now);
        let mut bytes = serde_json::to_vec(event)
            .map_err(|error| audit_error("serialize record", None, error))?;
        bytes.push(b'\n');
        if bytes.len() > MAX_AUDIT_RECORD_BYTES {
            return Err(CoshError::new(
                ErrorCode::AuditLogError,
                format!("audit record exceeds {} byte limit", MAX_AUDIT_RECORD_BYTES),
                "audit",
            ));
        }

        let needs_rotation = self.active.as_ref().is_some_and(|active| {
            active.bytes.saturating_add(bytes.len() as u64) > MAX_SEGMENT_BYTES
                || active.date != utc_date(now)
        });
        if needs_rotation {
            self.close_active()?;
            self.ensure_active(now)?;
            let sequence = self
                .active
                .as_ref()
                .map(|active| active.sequence)
                .unwrap_or(0);
            event.assign_writer_fields(sequence, now);
            bytes = serde_json::to_vec(event)
                .map_err(|error| audit_error("serialize record", None, error))?;
            bytes.push(b'\n');
        }

        let active = self.active.as_mut().ok_or_else(|| {
            CoshError::new(
                ErrorCode::AuditUnavailable,
                "audit segment is unavailable after initialization",
                "audit",
            )
        })?;
        active
            .file
            .write_all(&bytes)
            .map_err(|error| io_error("write segment", &active.active_path, error))?;
        active.bytes = active.bytes.saturating_add(bytes.len() as u64);
        active.sequence = active.sequence.saturating_add(1);
        // Flushing every record is within the ordinary "at most one second or
        // eight records" bound and avoids a background timer owning the writer.
        active
            .file
            .flush()
            .map_err(|error| io_error("flush segment", &active.active_path, error))?;
        if durability == AuditDurability::SecurityBoundary {
            active
                .file
                .get_ref()
                .sync_data()
                .map_err(|error| io_error("sync segment", &active.active_path, error))?;
        }
        Ok(())
    }

    /// Flushes, syncs, and renames the active segment while its lock is held.
    ///
    /// # Errors
    ///
    /// Returns a safe audit error when flush, sync, or rename fails.
    pub fn close(&mut self) -> Result<(), CoshError> {
        self.close_active()
    }

    /// Returns the current active segment basename for diagnostics.
    pub fn active_basename(&self) -> Option<&str> {
        self.active
            .as_ref()
            .and_then(|active| active.active_path.file_name())
            .and_then(|name| name.to_str())
    }

    fn ensure_active(&mut self, now: DateTime<Utc>) -> Result<(), CoshError> {
        if self.active.is_some() {
            return Ok(());
        }
        let date = utc_date(now);
        let directory = self.root.join("v1/segments").join(&date);
        ensure_private_dir(&directory)?;
        let id = Uuid::new_v4().to_string();
        let start_ms = now.timestamp_millis();
        let basename = format!(
            "{}-{start_ms}-{}-{id}.jsonl.active",
            self.component.as_str(),
            std::process::id()
        );
        let path = directory.join(basename);
        let file = create_private_file(&path)?;
        let file = Flock::lock(file, FlockArg::LockExclusiveNonblock).map_err(|(_, error)| {
            CoshError::new(
                ErrorCode::AuditUnavailable,
                format!("lock segment {}: {error}", safe_basename(&path)),
                "audit",
            )
        })?;
        self.active = Some(ActiveSegment {
            date,
            sequence: 0,
            bytes: 0,
            active_path: path,
            file: BufWriter::new(LockedFile(file)),
        });
        Ok(())
    }

    fn close_active(&mut self) -> Result<(), CoshError> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        active
            .file
            .flush()
            .map_err(|error| io_error("flush segment", &active.active_path, error))?;
        active
            .file
            .get_ref()
            .sync_data()
            .map_err(|error| io_error("sync segment", &active.active_path, error))?;
        let closed = closed_path(&active.active_path)?;
        close_without_replace(&active.active_path, &closed)
            .map_err(|error| io_error("close segment", &active.active_path, error))?;
        drop(active);
        Ok(())
    }
}

impl Drop for AuditSegmentWriter {
    fn drop(&mut self) {
        if let Err(error) = self.close_active() {
            tracing::warn!(target: "cosh_audit", "failed to close audit segment: {error}");
        }
    }
}

/// Creates a private file with create-new and no-follow semantics.
///
/// # Errors
///
/// Returns a safe audit error when creation or post-open validation fails.
pub fn create_private_file(path: &Path) -> Result<File, CoshError> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    let file = options
        .open(path)
        .map_err(|error| io_error("create segment", path, error))?;
    validate_private_file(&file, path)?;
    Ok(file)
}

/// Validates that an open file is regular, private, and owned by the process user.
///
/// # Errors
///
/// Returns a safe audit error for unsafe file metadata.
pub fn validate_private_file(file: &File, path: &Path) -> Result<(), CoshError> {
    let metadata = file
        .metadata()
        .map_err(|error| io_error("inspect segment", path, error))?;
    if !metadata.file_type().is_file() {
        return Err(unsafe_path("segment is not a regular file", path));
    }
    #[cfg(unix)]
    {
        if metadata.uid() != nix::unistd::Uid::effective().as_raw() {
            return Err(unsafe_path("segment owner does not match euid", path));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(unsafe_path("segment permissions are not private", path));
        }
    }
    Ok(())
}

/// Creates or validates one private directory.
///
/// # Errors
///
/// Returns a safe audit error for symlinks, non-directories, owner mismatch,
/// unsafe permissions, or creation failures.
pub fn ensure_private_dir(path: &Path) -> Result<(), CoshError> {
    #[cfg(unix)]
    {
        ensure_private_dir_unix(path)
    }
    #[cfg(not(unix))]
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => validate_private_dir_metadata(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path)
                .map_err(|error| io_error("create audit directory", path, error))?;
            let metadata = std::fs::symlink_metadata(path)
                .map_err(|error| io_error("inspect audit directory", path, error))?;
            validate_private_dir_metadata(path, &metadata)
        }
        Err(error) => Err(io_error("inspect audit directory", path, error)),
    }
}

#[cfg(unix)]
fn ensure_private_dir_unix(path: &Path) -> Result<(), CoshError> {
    use nix::dir::Dir;
    use nix::errno::Errno;
    use nix::fcntl::OFlag;
    use nix::sys::stat::{fstat, mkdirat, Mode, SFlag};

    validate_root_path(path)?;
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let mode = Mode::from_bits_truncate(0o700);
    let mut directory = Dir::open("/", flags, Mode::empty())
        .map_err(|error| audit_error("open audit ancestor", Some(path), error))?;
    let names = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(Ok(name)),
            Component::RootDir => None,
            _ => Some(Err(unsafe_path(
                "audit root contains unsafe traversal",
                path,
            ))),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if names.is_empty() {
        return Err(unsafe_path("audit root cannot be filesystem root", path));
    }

    for (index, name) in names.iter().enumerate() {
        // Each component is opened relative to the already verified parent, so
        // a concurrent symlink swap cannot redirect creation outside the root.
        let opened = Dir::openat(Some(directory.as_raw_fd()), *name, flags, Mode::empty());
        let next = match opened {
            Ok(next) => next,
            Err(Errno::ENOENT) => {
                if let Err(error) = mkdirat(Some(directory.as_raw_fd()), *name, mode) {
                    if error != Errno::EEXIST {
                        return Err(audit_error("create audit directory", Some(path), error));
                    }
                }
                Dir::openat(Some(directory.as_raw_fd()), *name, flags, Mode::empty())
                    .map_err(|_| unsafe_path("audit ancestor is not a real directory", path))?
            }
            Err(_) => {
                return Err(unsafe_path("audit ancestor is not a real directory", path));
            }
        };
        directory = next;
        if index + 1 == names.len() {
            let metadata = fstat(directory.as_raw_fd())
                .map_err(|error| audit_error("inspect audit directory", Some(path), error))?;
            if SFlag::from_bits_truncate(metadata.st_mode) != SFlag::S_IFDIR
                || metadata.st_uid != nix::unistd::Uid::effective().as_raw()
                || metadata.st_mode & 0o077 != 0
            {
                return Err(unsafe_path("audit directory owner or mode is unsafe", path));
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_dir_metadata(
    path: &Path,
    metadata: &std::fs::Metadata,
) -> Result<(), CoshError> {
    if metadata.file_type().is_symlink() || !metadata.file_type().is_dir() {
        return Err(unsafe_path("audit path is not a real directory", path));
    }
    #[cfg(unix)]
    {
        if metadata.uid() != nix::unistd::Uid::effective().as_raw() {
            return Err(unsafe_path(
                "audit directory owner does not match euid",
                path,
            ));
        }
        if metadata.mode() & 0o077 != 0 {
            return Err(unsafe_path(
                "audit directory permissions are not private",
                path,
            ));
        }
    }
    Ok(())
}

fn validate_root_path(path: &Path) -> Result<(), CoshError> {
    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(unsafe_path("audit root contains unsafe traversal", path));
    }
    Ok(())
}

pub(crate) fn close_without_replace(active: &Path, closed: &Path) -> std::io::Result<()> {
    std::fs::hard_link(active, closed)?;
    if let Err(error) = std::fs::remove_file(active) {
        let _ = std::fs::remove_file(closed);
        return Err(error);
    }
    Ok(())
}

fn closed_path(active: &Path) -> Result<PathBuf, CoshError> {
    let basename = active
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| unsafe_path("segment basename is not UTF-8", active))?;
    let closed = basename
        .strip_suffix(".active")
        .ok_or_else(|| unsafe_path("active segment suffix is invalid", active))?;
    Ok(active.with_file_name(closed))
}

fn utc_date(timestamp: DateTime<Utc>) -> String {
    timestamp.format("%Y-%m-%d").to_string()
}

fn safe_basename(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("<invalid-basename>")
        .to_string()
}

fn audit_error(operation: &str, path: Option<&Path>, error: impl std::fmt::Display) -> CoshError {
    let location = path
        .map(safe_basename)
        .map(|name| format!(" {name}"))
        .unwrap_or_default();
    CoshError::new(
        ErrorCode::AuditLogError,
        format!("{operation}{location}: {error}"),
        "audit",
    )
}

fn io_error(operation: &str, path: &Path, error: std::io::Error) -> CoshError {
    audit_error(operation, Some(path), error)
}

fn unsafe_path(reason: &str, path: &Path) -> CoshError {
    CoshError::new(
        ErrorCode::AuditUnavailable,
        format!("{reason}: {}", safe_basename(path)),
        "audit",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audit::AuditTestDir;
    use cosh_types::audit::{
        AuditActor, AuditActorKind, AuditComponent, AuditControlData, AuditEventOutcome,
        AuditEventType, AuditIdentity, AuditOutcomeStatus, AuditRedaction, AuditRedactionStatus,
        AuditSubject, KnownAuditEventType,
    };
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn event() -> AuditEventV1 {
        let now = Utc::now();
        AuditEventV1::new(
            Uuid::new_v4().to_string(),
            AuditEventType::from(KnownAuditEventType::AuditDegraded),
            now,
            now,
            99,
            AuditComponent {
                name: AuditComponentName::CoshCli,
                version: "0.12.0".to_string(),
            },
            AuditIdentity::default(),
            AuditActor {
                kind: AuditActorKind::System,
                uid: None,
                euid: None,
            },
            AuditEventOutcome {
                status: AuditOutcomeStatus::Degraded,
                code: Some("write_failed".to_string()),
                retryable: true,
            },
            AuditSubject {
                kind: "audit".to_string(),
                name: None,
            },
            &AuditControlData::default(),
            AuditRedaction {
                policy_version: "audit-redaction-v1".to_string(),
                status: AuditRedactionStatus::Clean,
                fields: Vec::new(),
            },
        )
        .unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn writer_uses_distinct_locked_segments_and_closes_them() {
        let root = AuditTestDir::create();
        let mut first =
            AuditSegmentWriter::create(root.path(), AuditComponentName::CoshCli).unwrap();
        let mut second =
            AuditSegmentWriter::create(root.path(), AuditComponentName::CoshCli).unwrap();
        first
            .append(&mut event(), AuditDurability::SecurityBoundary)
            .unwrap();
        second
            .append(&mut event(), AuditDurability::SecurityBoundary)
            .unwrap();
        assert_ne!(first.active_basename(), second.active_basename());
        first.close().unwrap();
        second.close().unwrap();
        let date = utc_date(Utc::now());
        let names: Vec<_> = std::fs::read_dir(root.path().join("v1/segments").join(date))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2);
        assert!(names.iter().all(|name| name.ends_with(".jsonl")));
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_existing_directory_mode_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(AuditSegmentWriter::create(root.path(), AuditComponentName::CoshCli).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn private_files_are_create_new_and_mode_0600() {
        let root = AuditTestDir::create();
        let path = root.path().join("record");
        let file = create_private_file(&path).unwrap();
        let mode = file.metadata().unwrap().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(create_private_file(&path).is_err());
    }
}
