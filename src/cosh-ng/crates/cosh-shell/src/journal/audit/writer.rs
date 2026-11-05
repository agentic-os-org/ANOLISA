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

mod budget;

const MAX_SEGMENT_BYTES: u64 = 16 * 1024 * 1024;

pub(super) struct AuditSegmentWriter {
    root: PathBuf,
    active: Option<ActiveSegment>,
    // The segment buffer must drop before its shared reservation lock.
    budget_guard: Option<Flock<File>>,
    max_disk_bytes: u64,
    #[cfg(test)]
    failing_syncs: u32,
    #[cfg(test)]
    failing_stages: u32,
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
    #[cfg(test)]
    pub(super) fn create(root: &Path) -> Result<Self, String> {
        Self::create_with_capacity(
            root,
            crate::config::audit::AuditSettings::default().max_disk_bytes,
        )
    }

    pub(super) fn create_with_capacity(root: &Path, max_disk_bytes: u64) -> Result<Self, String> {
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
            budget_guard: None,
            max_disk_bytes,
            #[cfg(test)]
            failing_syncs: 0,
            #[cfg(test)]
            failing_stages: 0,
        })
    }

    pub(super) fn append(&mut self, event: &mut AuditEventV1, durable: bool) -> Result<(), String> {
        self.stage(event)?;
        self.commit(durable)
    }

    /// Buffers one record in the active segment without flushing it.
    ///
    /// On error the record has not entered the segment, unless it is at
    /// least as large as the segment buffer and its direct write tore.
    pub(super) fn stage(&mut self, event: &mut AuditEventV1) -> Result<(), String> {
        self.ensure_budget_lock()?;
        let result = self.stage_locked(event);
        self.release_budget_if_flushed();
        result
    }

    fn ensure_budget_lock(&mut self) -> Result<(), String> {
        if self.budget_guard.is_none() {
            self.budget_guard = Some(budget::lock(&self.root).map_err(safe_io_error)?);
        }
        Ok(())
    }

    fn release_budget_if_flushed(&mut self) {
        if self
            .active
            .as_ref()
            .is_none_or(|active| active.file.buffer().is_empty())
        {
            self.budget_guard = None;
        }
    }

    fn stage_locked(&mut self, event: &mut AuditEventV1) -> Result<(), String> {
        event.validate()?;
        #[cfg(test)]
        if self.failing_stages > 0 {
            self.failing_stages -= 1;
            return Err("audit I/O failed: injected stage failure".to_string());
        }
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
        if let Some(active) = self.active.as_mut() {
            active.bytes = active
                .file
                .get_ref()
                .0
                .metadata()
                .map_err(safe_io_error)?
                .len()
                .saturating_add(active.file.buffer().len() as u64);
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
        self.ensure_budget_lock()?;
        let buffered = self
            .active
            .as_ref()
            .map_or(0, |active| active.file.buffer().len() as u64);
        budget::admit(
            &self.root,
            self.budget_guard
                .as_ref()
                .ok_or("missing audit budget lock")?,
            &self
                .active
                .as_ref()
                .ok_or("missing audit segment")?
                .file
                .get_ref()
                .0,
            self.max_disk_bytes,
            buffered,
            bytes.len() as u64,
        )
        .map_err(safe_io_error)?;
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| "audit segment unavailable".to_string())?;
        active.file.write_all(&bytes).map_err(safe_io_error)?;
        active.bytes = active.bytes.saturating_add(bytes.len() as u64);
        active.sequence = active.sequence.saturating_add(1);
        Ok(())
    }

    /// Flushes staged records and, when `durable`, syncs the active segment.
    ///
    /// Staged records stay in the segment when this fails, so a caller
    /// retries this step rather than staging the same record again.
    pub(super) fn commit(&mut self, durable: bool) -> Result<(), String> {
        let result = self.commit_locked(durable);
        self.release_budget_if_flushed();
        result
    }

    fn commit_locked(&mut self, durable: bool) -> Result<(), String> {
        let active = self
            .active
            .as_mut()
            .ok_or_else(|| "audit segment unavailable".to_string())?;
        active.file.flush().map_err(safe_io_error)?;
        if durable {
            #[cfg(test)]
            if self.failing_syncs > 0 {
                self.failing_syncs -= 1;
                return Err("audit I/O failed: injected sync failure".to_string());
            }
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
        if self.active.is_none() {
            return Ok(());
        }
        self.commit(true)?;
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "audit segment unavailable".to_string())?;
        let metadata = active.file.get_ref().0.metadata().map_err(safe_io_error)?;
        if metadata.len() == 0 {
            let current = std::fs::symlink_metadata(&active.path).map_err(safe_io_error)?;
            #[cfg(unix)]
            if (current.dev(), current.ino()) != (metadata.dev(), metadata.ino()) {
                return Err("empty audit segment identity changed".to_string());
            }
            std::fs::remove_file(&active.path).map_err(safe_io_error)?;
            self.active = None;
            return Ok(());
        }
        let basename = active
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(|name| name.strip_suffix(".active"))
            .ok_or_else(|| "invalid active audit segment name".to_string())?;
        let closed = active.path.with_file_name(basename);
        no_replace_close(&active.path, &closed).map_err(safe_io_error)?;
        self.active = None;
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn active_path(&self) -> Option<&Path> {
        self.active.as_ref().map(|active| active.path.as_path())
    }

    /// Fails the next `count` syncs after their records were flushed.
    #[cfg(test)]
    pub(super) fn fail_next_syncs(&mut self, count: u32) {
        self.failing_syncs = count;
    }

    /// Fails the next `count` stages before any record bytes enter the segment.
    #[cfg(test)]
    pub(super) fn fail_next_stages(&mut self, count: u32) {
        self.failing_stages = count;
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

#[cfg(test)]
mod budget_tests {
    use super::*;
    use crate::types::audit::{
        AuditEventOutcome, AuditIdentity, AuditOutcomeStatus, AuditRedaction, AuditSubject,
    };
    use std::os::unix::fs::PermissionsExt;

    fn root() -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        root
    }

    fn event() -> AuditEventV1 {
        AuditEventV1::shell(
            "session.started",
            AuditIdentity::default(),
            AuditEventOutcome {
                status: AuditOutcomeStatus::Started,
                code: None,
                retryable: false,
            },
            AuditSubject {
                kind: "session".into(),
                name: None,
            },
            &serde_json::json!({}),
            AuditRedaction::clean(),
        )
        .unwrap()
    }

    #[test]
    fn rejected_short_lived_writers_leave_no_empty_segments() {
        let root = root();
        let path = root.path().canonicalize().unwrap();
        let mut keeper = AuditSegmentWriter::create_with_capacity(&path, 4096).unwrap();
        keeper.append(&mut event(), true).unwrap();
        let kept_path = keeper.active_path().unwrap().to_path_buf();
        let before = std::fs::read(&kept_path).unwrap();
        keeper.close().unwrap();
        let closed = kept_path.with_extension("");
        for _ in 0..16 {
            let mut writer = AuditSegmentWriter::create_with_capacity(&path, 1).unwrap();
            assert!(writer.append(&mut event(), true).is_err());
        }
        let directory = closed.parent().unwrap();
        let segments = std::fs::read_dir(directory).unwrap().collect::<Vec<_>>();
        assert_eq!(
            segments.len(),
            1,
            "rejected writers must not publish empty history"
        );
        assert_eq!(std::fs::read(closed).unwrap(), before);
    }

    #[test]
    fn close_refuses_to_remove_a_replaced_empty_segment() {
        let root = root();
        let path = root.path().canonicalize().unwrap();
        let mut writer = AuditSegmentWriter::create_with_capacity(&path, 1).unwrap();
        assert!(writer.append(&mut event(), true).is_err());
        let active = writer.active_path().unwrap().to_path_buf();
        let original = active.with_extension("original");
        std::fs::rename(&active, &original).unwrap();
        std::fs::write(&active, b"replacement").unwrap();
        assert!(writer.close().is_err());
        drop(writer);
        assert_eq!(std::fs::read(active).unwrap(), b"replacement");
        assert_eq!(std::fs::metadata(original).unwrap().len(), 0);
    }

    #[test]
    fn close_flushes_buffered_records_before_checking_for_empty_segments() {
        let root = root();
        let path = root.path().canonicalize().unwrap();
        let mut writer = AuditSegmentWriter::create_with_capacity(&path, 4096).unwrap();
        let mut record = event();
        writer.stage(&mut record).unwrap();
        let active = writer.active_path().unwrap().to_path_buf();
        assert_eq!(std::fs::metadata(&active).unwrap().len(), 0);
        assert!(!writer.active.as_ref().unwrap().file.buffer().is_empty());
        writer.close().unwrap();
        assert!(!active.exists());
        let bytes = std::fs::read(active.with_extension("")).unwrap();
        let stored: AuditEventV1 = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(stored.event_id, record.event_id);
    }

    #[test]
    fn staged_bytes_keep_the_shared_lock_until_commit() {
        let root = root();
        let path = root.path().canonicalize().unwrap();
        let mut first = AuditSegmentWriter::create_with_capacity(&path, 4096).unwrap();
        let mut second = AuditSegmentWriter::create_with_capacity(&path, 4096).unwrap();
        first.stage(&mut event()).unwrap();
        assert!(second.append(&mut event(), true).is_err());
        first.commit(true).unwrap();
        second.append(&mut event(), true).unwrap();
        assert!(
            first
                .active
                .as_ref()
                .unwrap()
                .file
                .get_ref()
                .0
                .metadata()
                .unwrap()
                .len()
                > 0
        );
        assert!(
            second
                .active
                .as_ref()
                .unwrap()
                .file
                .get_ref()
                .0
                .metadata()
                .unwrap()
                .len()
                > 0
        );
    }

    #[test]
    fn flush_failure_keeps_pending_bytes_reserved_until_writer_drops() {
        let root = root();
        let path = root.path().canonicalize().unwrap();
        let mut first = AuditSegmentWriter::create_with_capacity(&path, 4096).unwrap();
        first.ensure_active(Utc::now()).unwrap();
        let mut active = first.active.take().unwrap();
        let old_file = active.file.into_inner().map_err(|_| ()).unwrap();
        drop(old_file);
        let readonly = OpenOptions::new().read(true).open(&active.path).unwrap();
        active.file = BufWriter::new(LockedFile(
            Flock::lock(readonly, FlockArg::LockExclusiveNonblock).unwrap(),
        ));
        first.active = Some(active);
        assert!(first.append(&mut event(), true).is_err());
        assert!(!first.active.as_ref().unwrap().file.buffer().is_empty());
        let mut second = AuditSegmentWriter::create_with_capacity(&path, 4096).unwrap();
        assert!(second.append(&mut event(), true).is_err());
        drop(first);
        second.append(&mut event(), true).unwrap();
    }

    #[test]
    fn configured_capacity_counts_all_live_segments() {
        let root = root();
        let path = root.path().canonicalize().unwrap();
        let mut first = AuditSegmentWriter::create_with_capacity(&path, 4096).unwrap();
        let mut second = AuditSegmentWriter::create_with_capacity(&path, 4096).unwrap();
        let mut accepted = 0;
        for index in 0..100 {
            let current = if index % 2 == 0 {
                &mut first
            } else {
                &mut second
            };
            if current.append(&mut event(), false).is_err() {
                break;
            }
            accepted += 1;
        }
        assert!(accepted > 1 && accepted < 100);
        let first_bytes = first
            .active
            .as_ref()
            .unwrap()
            .file
            .get_ref()
            .0
            .metadata()
            .unwrap()
            .len();
        let second_bytes = second
            .active
            .as_ref()
            .unwrap()
            .file
            .get_ref()
            .0
            .metadata()
            .unwrap()
            .len();
        assert!(first_bytes + second_bytes <= 4096);
        let mut larger = AuditSegmentWriter::create_with_capacity(&path, 8192).unwrap();
        larger.append(&mut event(), true).unwrap();
    }

    #[test]
    fn replaced_segments_cannot_hide_the_active_descriptor() {
        let root = root();
        let path = root.path().canonicalize().unwrap();
        let mut writer = AuditSegmentWriter::create_with_capacity(&path, 4096).unwrap();
        writer.append(&mut event(), true).unwrap();
        let active_path = writer.active.as_ref().unwrap().path.clone();
        let before = std::fs::metadata(&active_path).unwrap().len();
        std::fs::rename(path.join("v1/segments"), path.join("hidden")).unwrap();
        let empty = path.join("empty");
        std::fs::create_dir(&empty).unwrap();
        std::fs::set_permissions(&empty, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&empty, path.join("v1/segments")).unwrap();
        assert!(writer.append(&mut event(), true).is_err());
        std::fs::remove_file(path.join("v1/segments")).unwrap();
        std::fs::rename(path.join("hidden"), path.join("v1/segments")).unwrap();
        assert_eq!(std::fs::metadata(active_path).unwrap().len(), before);
        writer.append(&mut event(), true).unwrap();
    }
}
