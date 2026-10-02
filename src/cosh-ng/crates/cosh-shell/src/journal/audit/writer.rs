//! Private segment storage used by the Shell audit recorder.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use nix::fcntl::{Flock, FlockArg};

use crate::types::audit::{AuditEventV1, MAX_AUDIT_RECORD_BYTES};

#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

const MAX_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;

pub(super) struct AuditSegmentWriter {
    root: PathBuf,
    active: Option<ActiveSegment>,
}

struct ActiveSegment {
    date: String,
    sequence: u64,
    bytes: u64,
    path: PathBuf,
    file: BufWriter<LockedFile>,
}

struct LockedFile(Flock<File>);

impl Write for LockedFile {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.0.write(buffer)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

impl AuditSegmentWriter {
    pub(super) fn create(root: &Path) -> Result<Self, String> {
        if !root.is_absolute()
            || root.as_os_str().is_empty()
            || root
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err("unsafe audit root".to_string());
        }
        ensure_private_dir(root)?;
        ensure_private_dir(&root.join("v1"))?;
        ensure_private_dir(&root.join("v1/segments"))?;
        Ok(Self {
            root: root.to_path_buf(),
            active: None,
        })
    }

    pub(super) fn append(&mut self, event: &mut AuditEventV1, durable: bool) -> Result<(), String> {
        event.validate()?;
        self.ensure_active(Utc::now())?;
        let now = Utc::now();
        let sequence = self.active.as_ref().map_or(0, |active| active.sequence);
        event.sequence = sequence;
        event.observed_at = now;
        let mut bytes = serde_json::to_vec(event).map_err(|error| error.to_string())?;
        bytes.push(b'\n');
        if bytes.len() > MAX_AUDIT_RECORD_BYTES {
            return Err("audit record exceeds 65536 byte limit".to_string());
        }
        let rotate = self.active.as_ref().is_some_and(|active| {
            active.date != utc_date(now)
                || active.bytes.saturating_add(bytes.len() as u64) > MAX_SEGMENT_BYTES
        });
        if rotate {
            self.close()?;
            self.ensure_active(now)?;
            let sequence = self.active.as_ref().map_or(0, |active| active.sequence);
            event.sequence = sequence;
            event.observed_at = now;
            bytes = serde_json::to_vec(event).map_err(|error| error.to_string())?;
            bytes.push(b'\n');
        }
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| "audit segment unavailable".to_string())?;
        active.file.write_all(&bytes).map_err(safe_io_error)?;
        active.bytes = active.bytes.saturating_add(bytes.len() as u64);
        active.sequence = active.sequence.saturating_add(1);
        active.file.flush().map_err(safe_io_error)?;
        if durable {
            active.file.get_ref().0.sync_data().map_err(safe_io_error)?;
        }
        Ok(())
    }

    fn ensure_active(&mut self, now: DateTime<Utc>) -> Result<(), String> {
        if self.active.is_some() {
            return Ok(());
        }
        let date = utc_date(now);
        let directory = self.root.join("v1/segments").join(&date);
        ensure_private_dir(&directory)?;
        let basename = format!(
            "cosh-shell-{}-{}-{}.jsonl.active",
            now.timestamp_millis(),
            std::process::id(),
            uuid::Uuid::new_v4()
        );
        let path = directory.join(basename);
        let file = create_private_file(&path)?;
        let file = Flock::lock(file, FlockArg::LockExclusiveNonblock)
            .map_err(|(_, error)| format!("lock audit segment: {error}"))?;
        self.active = Some(ActiveSegment {
            date,
            sequence: 0,
            bytes: 0,
            path,
            file: BufWriter::new(LockedFile(file)),
        });
        Ok(())
    }

    pub(super) fn close(&mut self) -> Result<(), String> {
        let Some(mut active) = self.active.take() else {
            return Ok(());
        };
        active.file.flush().map_err(safe_io_error)?;
        active.file.get_ref().0.sync_data().map_err(safe_io_error)?;
        let basename = active
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".active"))
            .ok_or_else(|| "invalid active audit segment name".to_string())?;
        let closed = active.path.with_file_name(basename);
        no_replace_close(&active.path, &closed).map_err(safe_io_error)?;
        drop(active);
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn active_path(&self) -> Option<&Path> {
        self.active.as_ref().map(|active| active.path.as_path())
    }
}

fn no_replace_close(active: &Path, closed: &Path) -> std::io::Result<()> {
    std::fs::hard_link(active, closed)?;
    if let Err(error) = std::fs::remove_file(active) {
        let _ = std::fs::remove_file(closed);
        return Err(error);
    }
    Ok(())
}

impl Drop for AuditSegmentWriter {
    fn drop(&mut self) {
        let _ = self.close();
    }
}

fn create_private_file(path: &Path) -> Result<File, String> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600).custom_flags(nix::libc::O_NOFOLLOW);
    let file = options.open(path).map_err(safe_io_error)?;
    let metadata = file.metadata().map_err(safe_io_error)?;
    if !metadata.file_type().is_file() {
        return Err("audit segment is not a regular file".to_string());
    }
    #[cfg(unix)]
    if metadata.uid() != nix::unistd::Uid::effective().as_raw() || metadata.mode() & 0o077 != 0 {
        return Err("audit segment owner or mode is unsafe".to_string());
    }
    Ok(file)
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        ensure_private_dir_unix(path)
    }
    #[cfg(not(unix))]
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err("audit directory is not a real directory".to_string());
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(path).map_err(safe_io_error)?;
            ensure_private_dir(path)
        }
        Err(error) => Err(safe_io_error(error)),
    }
}

#[cfg(unix)]
fn ensure_private_dir_unix(path: &Path) -> Result<(), String> {
    use nix::dir::Dir;
    use nix::errno::Errno;
    use nix::fcntl::OFlag;
    use nix::sys::stat::{fstat, mkdirat, Mode, SFlag};

    if !path.is_absolute()
        || path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return Err("unsafe audit root".to_string());
    }
    let names = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(Ok(name)),
            Component::RootDir => None,
            _ => Some(Err("unsafe audit root".to_string())),
        })
        .collect::<Result<Vec<_>, _>>()?;
    if names.is_empty() {
        return Err("audit root cannot be filesystem root".to_string());
    }
    let flags = OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let mode = Mode::from_bits_truncate(0o700);
    let mut directory = Dir::open("/", flags, Mode::empty())
        .map_err(|error| format!("open audit ancestor: {error}"))?;
    for (index, name) in names.iter().enumerate() {
        let next = match Dir::openat(Some(directory.as_raw_fd()), *name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(Errno::ENOENT) => {
                if let Err(error) = mkdirat(Some(directory.as_raw_fd()), *name, mode) {
                    if error != Errno::EEXIST {
                        return Err(format!("create audit directory: {error}"));
                    }
                }
                Dir::openat(Some(directory.as_raw_fd()), *name, flags, Mode::empty())
                    .map_err(|_| "audit ancestor is not a real directory".to_string())?
            }
            Err(_) => return Err("audit ancestor is not a real directory".to_string()),
        };
        directory = next;
        if index + 1 == names.len() {
            let metadata = fstat(directory.as_raw_fd())
                .map_err(|error| format!("inspect audit directory: {error}"))?;
            if SFlag::from_bits_truncate(metadata.st_mode) != SFlag::S_IFDIR
                || metadata.st_uid != nix::unistd::Uid::effective().as_raw()
                || metadata.st_mode & 0o077 != 0
            {
                return Err("audit directory owner or mode is unsafe".to_string());
            }
        }
    }
    Ok(())
}

fn utc_date(timestamp: DateTime<Utc>) -> String {
    timestamp.format("%Y-%m-%d").to_string()
}

fn safe_io_error(error: std::io::Error) -> String {
    format!("audit I/O failed: {error}")
}
