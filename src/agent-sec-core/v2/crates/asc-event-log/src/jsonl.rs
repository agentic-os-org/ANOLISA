//! Rotation-aware, cross-process-safe `JSONL` append writer.
//!
//! Migrated from v1 `security_events/writer.py`. Four properties are load
//! bearing and were taken from the v1 source rather than from prose:
//!
//! * The rotation predicate is `size + line_bytes >= max_bytes`, i.e. it
//!   rotates on *reaching* the limit, not on exceeding it.
//! * Backup pruning sorts candidates by **mtime**, not by name.
//! * The advisory lock lives in a dedicated `<name>.lock` file, and the log
//!   file is reopened by path inside the critical section so no stale fd can
//!   reference a recycled inode.
//! * A failure to take the lock does **not** fail the write: v1 falls through
//!   and writes unlocked, accepting a small race.

use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::Utc;
use rustix::fs::{FlockOperation, Mode, OFlags, flock};
use serde::Serialize;

use crate::error::EventLogError;

/// Default maximum log file size before rotation (100 MB).
pub const DEFAULT_MAX_BYTES: u64 = 100 * 1024 * 1024;

/// Default number of rotated files to keep.
pub const DEFAULT_BACKUP_COUNT: usize = 10;

/// Default diagnostic prefix used by the security-events stream.
pub const DEFAULT_ERROR_PREFIX: &str = "[security_events]";

/// Owner-only mode enforced on data files and their lock files.
///
/// Local streams can contain request/result evidence, so the mode is applied
/// independently of the caller's umask.
const PRIVATE_FILE_MODE: u32 = 0o600;

/// [`PRIVATE_FILE_MODE`] expressed with `rustix` flags, so no numeric cast is
/// needed on platforms where the raw mode type is not `u32`.
const PRIVATE_MODE: Mode = Mode::RUSR.union(Mode::WUSR);

/// Mask covering the permission and set-id bits, i.e. `0o7777`.
const PERMISSION_BITS: Mode = Mode::RWXU
    .union(Mode::RWXG)
    .union(Mode::RWXO)
    .union(Mode::SUID)
    .union(Mode::SGID)
    .union(Mode::SVTX);

/// Mode used when creating the parent directory.
const PRIVATE_DIR_MODE: u32 = 0o700;

/// Upper bound on the rotation collision counter, matching v1 `range(1, 1000)`.
const MAX_COLLISION_SEQ: u32 = 1000;

/// Callback invoked for swallowed writer failures.
pub type ErrorHandler = Box<dyn Fn(&EventLogError) + Send + Sync>;

/// Returns whether `suffix` is a rotation suffix this writer produced.
///
/// Equivalent to v1 `_BACKUP_SUFFIX_RE`, i.e. `^\d{8}-\d{6}\.\d{3}(\.\d+)?$`,
/// hand-rolled so the crate needs no regex dependency.
#[must_use]
pub fn is_backup_suffix(suffix: &str) -> bool {
    let mut parts = suffix.split('.');
    let Some(stamp) = parts.next() else {
        return false;
    };
    let Some((date, time)) = stamp.split_once('-') else {
        return false;
    };
    if !is_digits(date, 8) || !is_digits(time, 6) {
        return false;
    }
    let Some(millis) = parts.next() else {
        return false;
    };
    if !is_digits(millis, 3) {
        return false;
    }
    match parts.next() {
        None => true,
        Some(counter) => {
            !counter.is_empty()
                && counter.bytes().all(|b| b.is_ascii_digit())
                && parts.next().is_none()
        }
    }
}

fn is_digits(value: &str, expected_len: usize) -> bool {
    value.len() == expected_len && value.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Debug, Default)]
struct WriterState {
    dir_created: bool,
    retained_backups_secured: bool,
}

/// Appends JSON-serializable records to a `JSONL` file.
///
/// * **Thread-safe** — every write is serialized by a mutex, as v1 serializes
///   with a `threading.Lock`.
/// * **Auto-rotation** — rotates once the pending line would reach
///   `max_bytes`, keeping up to `backup_count` backups.
/// * **Cross-process safe** — a dedicated advisory lock file serializes
///   rotation *and* the subsequent write.
/// * **Fire-and-forget** — [`JsonlEventWriter::write`] swallows every failure;
///   [`JsonlEventWriter::write_or_raise`] surfaces them.
pub struct JsonlEventWriter {
    path: PathBuf,
    max_bytes: u64,
    backup_count: usize,
    error_prefix: String,
    on_error: Option<ErrorHandler>,
    state: Mutex<WriterState>,
}

impl std::fmt::Debug for JsonlEventWriter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonlEventWriter")
            .field("path", &self.path)
            .field("max_bytes", &self.max_bytes)
            .field("backup_count", &self.backup_count)
            .field("error_prefix", &self.error_prefix)
            .field("has_error_handler", &self.on_error.is_some())
            .field("state", &self.state)
            .finish()
    }
}

impl JsonlEventWriter {
    /// Creates a writer for `path` with the v1 default limits.
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            max_bytes: DEFAULT_MAX_BYTES,
            backup_count: DEFAULT_BACKUP_COUNT,
            error_prefix: DEFAULT_ERROR_PREFIX.to_owned(),
            on_error: None,
            state: Mutex::new(WriterState::default()),
        }
    }

    /// Overrides the rotation threshold.
    #[must_use]
    pub fn with_max_bytes(mut self, max_bytes: u64) -> Self {
        self.max_bytes = max_bytes;
        self
    }

    /// Overrides how many rotated backups are retained.
    #[must_use]
    pub fn with_backup_count(mut self, backup_count: usize) -> Self {
        self.backup_count = backup_count;
        self
    }

    /// Overrides the diagnostic prefix.
    #[must_use]
    pub fn with_error_prefix(mut self, error_prefix: impl Into<String>) -> Self {
        self.error_prefix = error_prefix.into();
        self
    }

    /// Installs a callback for failures swallowed by
    /// [`JsonlEventWriter::write`].
    ///
    /// The callback is never allowed to affect the write outcome, matching v1
    /// `_notify_error`, which wraps the callback in its own `try/except`.
    #[must_use]
    pub fn with_error_handler(mut self, handler: ErrorHandler) -> Self {
        self.on_error = Some(handler);
        self
    }

    /// Returns the log path.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the diagnostic prefix.
    #[must_use]
    pub fn error_prefix(&self) -> &str {
        &self.error_prefix
    }

    /// Returns the rotation threshold in bytes.
    #[must_use]
    pub const fn max_bytes(&self) -> u64 {
        self.max_bytes
    }

    /// Returns the retained backup count.
    #[must_use]
    pub const fn backup_count(&self) -> usize {
        self.backup_count
    }

    /// Returns the advisory lock path used to serialize writes.
    #[must_use]
    pub fn lock_path(&self) -> PathBuf {
        let mut name = self
            .path
            .file_name()
            .map(std::ffi::OsStr::to_os_string)
            .unwrap_or_default();
        name.push(".lock");
        self.path.with_file_name(name)
    }

    /// Appends `record` as one `JSONL` line, swallowing every failure.
    ///
    /// Safe to call from any thread and never panics or returns an error, so
    /// logging can never disrupt the caller.
    pub fn write<T: Serialize + ?Sized>(&self, record: &T) {
        if let Err(err) = self.write_or_raise(record) {
            self.notify_error(&err);
        }
    }

    /// Appends `record` as one `JSONL` line, surfacing failures.
    ///
    /// # Errors
    ///
    /// Returns [`EventLogError::Serialize`] if the record cannot be rendered,
    /// or [`EventLogError::Io`] if the log could not be opened or extended.
    pub fn write_or_raise<T: Serialize + ?Sized>(&self, record: &T) -> Result<(), EventLogError> {
        let mut line = serde_json::to_string(record)?;
        line.push('\n');
        let line_bytes = line.len() as u64;

        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.write_under_flock(&mut state, &line, line_bytes)
    }

    /// Creates and opens the target without appending a synthetic record.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the parent directory or private target file
    /// cannot be prepared.
    pub fn probe(&self) -> Result<(), EventLogError> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.ensure_parent_dir(&mut state)?;
        drop(open_private_append(&self.path)?);
        Ok(())
    }

    fn notify_error(&self, err: &EventLogError) {
        if let Some(handler) = &self.on_error {
            // Error reporting is secondary to the write path. Match v1's nested
            // `try/except`: a faulty callback cannot turn best-effort logging
            // into a business-operation failure.
            let _ = catch_unwind(AssertUnwindSafe(|| handler(err)));
        }
    }

    fn write_under_flock(
        &self,
        state: &mut WriterState,
        line: &str,
        line_bytes: u64,
    ) -> Result<(), EventLogError> {
        // Taking the lock is best effort: v1 falls through to an unlocked write
        // when the lock file cannot be opened or locked.
        let lock = self.acquire_lock(state);

        self.secure_retained_backups_once(state);

        self.ensure_parent_dir(state)?;
        let mut file = open_private_append(&self.path)?;

        if self.needs_rotation(&file, line_bytes)? {
            drop(file);
            self.rotate();
            file = open_private_append(&self.path)?;
        }

        let result = file
            .write_all(line.as_bytes())
            .and_then(|()| file.flush())
            .map_err(|err| EventLogError::io("append to", &self.path, err));

        drop(file);
        if let Some(lock) = lock {
            // Closing the descriptor releases the flock; unlock explicitly so
            // the ordering matches v1.
            let _ = flock(&lock, FlockOperation::Unlock);
        }
        result
    }

    fn acquire_lock(&self, state: &mut WriterState) -> Option<File> {
        if self.ensure_parent_dir(state).is_err() {
            return None;
        }
        let lock_file = open_private_append(&self.lock_path()).ok()?;
        flock(&lock_file, FlockOperation::LockExclusive).ok()?;
        Some(lock_file)
    }

    fn ensure_parent_dir(&self, state: &mut WriterState) -> Result<(), EventLogError> {
        if state.dir_created {
            return Ok(());
        }
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::DirBuilder::new()
                .recursive(true)
                .mode(PRIVATE_DIR_MODE)
                .create(parent)
                .map_err(|err| EventLogError::io("create directory", parent, err))?;
        }
        state.dir_created = true;
        Ok(())
    }

    /// Tightens backups left behind by older releases, once per writer.
    fn secure_retained_backups_once(&self, state: &mut WriterState) {
        if state.retained_backups_secured {
            return;
        }
        state.retained_backups_secured = true;

        for entry in self.backup_entries() {
            tighten_retained_backup(&entry);
        }
    }

    /// Returns whether appending would reach the size limit.
    ///
    /// v1 uses `>=`, so a line landing exactly on `max_bytes` rotates first.
    fn needs_rotation(&self, file: &File, additional_bytes: u64) -> Result<bool, EventLogError> {
        let size = file
            .metadata()
            .map(|meta| meta.len())
            .map_err(|err| EventLogError::io("stat", &self.path, err))?;
        Ok(size + additional_bytes >= self.max_bytes)
    }

    /// Returns the paths that match this writer's rotation suffix pattern.
    fn backup_entries(&self) -> Vec<PathBuf> {
        let Some(parent) = self.path.parent() else {
            return Vec::new();
        };
        let Some(base_name) = self.path.file_name().and_then(std::ffi::OsStr::to_str) else {
            return Vec::new();
        };
        let prefix = format!("{base_name}.");

        let Ok(entries) = fs::read_dir(parent) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let suffix = name.strip_prefix(&prefix)?;
                is_backup_suffix(suffix).then(|| entry.path())
            })
            .collect()
    }

    /// Renames the log aside with a millisecond-precision UTC suffix.
    fn rotate(&self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        let Some(base_name) = self.path.file_name().and_then(std::ffi::OsStr::to_str) else {
            return;
        };

        let timestamp = Utc::now().format("%Y%m%d-%H%M%S%.3f").to_string();
        let mut backup_path = parent.join(format!("{base_name}.{timestamp}"));

        // Guard against timestamp collisions. v1 keeps the colliding path when
        // all 999 counters are taken, so the rename overwrites; preserved here.
        if backup_path.exists() {
            for seq in 1..MAX_COLLISION_SEQ {
                let candidate = parent.join(format!("{base_name}.{timestamp}.{seq}"));
                if !candidate.exists() {
                    backup_path = candidate;
                    break;
                }
            }
        }

        if let Err(err) = fs::rename(&self.path, &backup_path) {
            // v1 reports the failed rotation and keeps writing to the current
            // file; dropping the diagnostic would hide a log that stopped
            // rotating and is now growing without bound.
            self.notify_error(&EventLogError::io("rotate", &self.path, err));
            return;
        }

        self.cleanup_old_backups();
    }

    /// Removes the oldest backups once the retained count is exceeded.
    ///
    /// Ordering is by mtime, matching v1 `_cleanup_old_backups`.
    fn cleanup_old_backups(&self) {
        let mut backups: Vec<(PathBuf, std::time::SystemTime)> = self
            .backup_entries()
            .into_iter()
            .filter_map(|path| {
                let metadata = fs::metadata(&path).ok()?;
                metadata.is_file().then_some(())?;
                Some((path, metadata.modified().ok()?))
            })
            .collect();

        backups.sort_by(|left, right| left.1.cmp(&right.1));

        while backups.len() > self.backup_count {
            let (oldest, _) = backups.remove(0);
            if let Err(err) = fs::remove_file(&oldest) {
                // Same reason as the failed rename: v1 reports it, and a
                // backup that cannot be removed means retention silently
                // stopped bounding the directory.
                self.notify_error(&EventLogError::io("remove backup", &oldest, err));
            }
        }
    }
}

/// Best-effort tighten of one recognized backup, without following links.
fn tighten_retained_backup(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    if !metadata.is_file() {
        return;
    }

    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK;
    let Ok(file) = rustix::fs::open(path, flags, Mode::empty()) else {
        return;
    };
    let Ok(stat) = rustix::fs::fstat(&file) else {
        return;
    };
    if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::RegularFile {
        return;
    }
    if Mode::from_raw_mode(stat.st_mode).intersection(PERMISSION_BITS) != PRIVATE_MODE {
        let _ = rustix::fs::fchmod(&file, PRIVATE_MODE);
    }
}

/// Opens a file for append, enforcing mode `0o600`.
///
/// The creation mode prevents a new file from starting with broader
/// permissions; the follow-up `fchmod` tightens files created by older
/// releases and restores owner access under an unusually strict umask.
fn open_private_append(path: &Path) -> Result<File, EventLogError> {
    let file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(PRIVATE_FILE_MODE)
        .open(path)
        .map_err(|err| EventLogError::io("open", path, err))?;

    let metadata = file
        .metadata()
        .map_err(|err| EventLogError::io("stat", path, err))?;
    if metadata.mode() & 0o7777 != PRIVATE_FILE_MODE {
        rustix::fs::fchmod(&file, PRIVATE_MODE)
            .map_err(|err| EventLogError::io("chmod", path, err.into()))?;
    }
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Read;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{Arc, atomic::AtomicUsize, atomic::Ordering};

    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    fn writer_in(dir: &TempDir) -> JsonlEventWriter {
        JsonlEventWriter::new(dir.path().join("stream.jsonl"))
    }

    fn read_lines(path: &Path) -> Vec<String> {
        let mut text = String::new();
        File::open(path)
            .expect("log must exist")
            .read_to_string(&mut text)
            .expect("readable");
        text.lines().map(str::to_owned).collect()
    }

    fn mode_of(path: &Path) -> u32 {
        fs::metadata(path).expect("must exist").mode() & 0o7777
    }

    fn backup_names(dir: &TempDir) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir.path())
            .expect("readable dir")
            .filter_map(Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| {
                name.strip_prefix("stream.jsonl.")
                    .is_some_and(is_backup_suffix)
            })
            .collect();
        names.sort_unstable();
        names
    }

    #[test]
    fn defaults_match_v1() {
        assert_eq!(DEFAULT_MAX_BYTES, 100 * 1024 * 1024);
        assert_eq!(DEFAULT_BACKUP_COUNT, 10);
        assert_eq!(DEFAULT_ERROR_PREFIX, "[security_events]");
    }

    #[test]
    fn probe_creates_an_empty_private_log() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir);

        writer.probe().expect("probe");

        assert!(writer.path().exists());
        assert_eq!(fs::metadata(writer.path()).expect("metadata").len(), 0);
        assert_eq!(mode_of(writer.path()), PRIVATE_FILE_MODE);
    }

    #[test]
    fn appends_one_line_per_record() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir);
        writer.write_or_raise(&json!({"a": 1})).expect("write");
        writer.write_or_raise(&json!({"b": 2})).expect("write");

        assert_eq!(read_lines(writer.path()), vec![r#"{"a":1}"#, r#"{"b":2}"#]);
    }

    /// v1 uses `ensure_ascii=False`, so non-ASCII stays literal.
    #[test]
    fn non_ascii_is_not_escaped() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir);
        writer
            .write_or_raise(&json!({"msg": "中文 ok"}))
            .expect("write");
        assert_eq!(read_lines(writer.path()), vec![r#"{"msg":"中文 ok"}"#]);
    }

    #[test]
    fn creates_parent_directory_and_private_files() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("nested/deeper/stream.jsonl");
        let writer = JsonlEventWriter::new(&path);
        writer.write_or_raise(&json!({"a": 1})).expect("write");

        assert_eq!(mode_of(&path), 0o600);
        assert_eq!(mode_of(&writer.lock_path()), 0o600);
        assert_eq!(
            mode_of(path.parent().expect("has parent")) & 0o077,
            0,
            "parent directory must not be group/world accessible"
        );
    }

    #[test]
    fn tightens_a_previously_loose_log_file() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir);
        fs::write(writer.path(), b"").expect("seed");
        fs::set_permissions(writer.path(), fs::Permissions::from_mode(0o644)).expect("chmod");

        writer.write_or_raise(&json!({"a": 1})).expect("write");
        assert_eq!(mode_of(writer.path()), 0o600);
    }

    /// v1 rotates when `size + line >= max_bytes`, i.e. on reaching the limit.
    #[test]
    fn rotation_triggers_on_reaching_the_limit_not_exceeding_it() {
        let dir = TempDir::new().expect("temp dir");
        let line = format!("{}\n", json!({"a": 1}));
        let line_len = u64::try_from(line.len()).expect("small");

        let writer = writer_in(&dir).with_max_bytes(line_len);
        writer.write_or_raise(&json!({"a": 1})).expect("write");

        // An empty log plus this line already reaches the limit, so v1 rotates
        // the (empty) file away first and leaves one backup behind.
        assert_eq!(backup_names(&dir).len(), 1);
        assert_eq!(read_lines(writer.path()), vec![r#"{"a":1}"#]);
    }

    #[test]
    fn no_rotation_below_the_limit() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir).with_max_bytes(1024);
        for _ in 0..5 {
            writer.write_or_raise(&json!({"a": 1})).expect("write");
        }
        assert!(backup_names(&dir).is_empty());
        assert_eq!(read_lines(writer.path()).len(), 5);
    }

    #[test]
    fn rotated_backups_keep_the_v1_name_shape_and_mode() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir).with_max_bytes(4);
        writer.write_or_raise(&json!({"a": 1})).expect("write");
        writer.write_or_raise(&json!({"a": 2})).expect("write");

        let names = backup_names(&dir);
        assert!(!names.is_empty(), "expected at least one backup");
        for name in &names {
            let suffix = name
                .strip_prefix("stream.jsonl.")
                .expect("prefix checked by filter");
            assert!(is_backup_suffix(suffix), "bad backup suffix {suffix}");
            assert_eq!(mode_of(&dir.path().join(name)), 0o600);
        }
    }

    #[test]
    fn same_millisecond_collisions_get_a_counter_suffix() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir).with_max_bytes(4).with_backup_count(50);
        // Many rotations in a tight loop force at least one same-millisecond
        // collision on any reasonably fast machine.
        for index in 0..40 {
            writer.write_or_raise(&json!({"a": index})).expect("write");
        }
        let names = backup_names(&dir);
        assert!(names.len() > 1);
        let counted = names
            .iter()
            .filter(|name| {
                name.strip_prefix("stream.jsonl.")
                    .is_some_and(|suffix| suffix.split('.').count() == 3)
            })
            .count();
        assert!(counted > 0, "expected a collision counter among {names:?}");
    }

    #[test]
    fn prunes_backups_beyond_the_retained_count() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir).with_max_bytes(4).with_backup_count(3);
        for index in 0..12 {
            writer.write_or_raise(&json!({"a": index})).expect("write");
        }
        assert_eq!(backup_names(&dir).len(), 3);
    }

    /// Pruning is by mtime, so an old-but-alphabetically-late backup goes first.
    #[test]
    fn pruning_orders_by_mtime_not_by_name() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir).with_max_bytes(4).with_backup_count(1);

        let stale = dir.path().join("stream.jsonl.99991231-235959.999");
        fs::write(&stale, b"old\n").expect("seed stale backup");
        let long_ago = std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
        filetime_set(&stale, long_ago);

        writer.write_or_raise(&json!({"a": 1})).expect("write");
        writer.write_or_raise(&json!({"a": 2})).expect("write");

        assert!(
            !stale.exists(),
            "the oldest backup by mtime must be pruned even though its name sorts last"
        );
    }

    /// Sets mtime without pulling in an extra dependency.
    fn filetime_set(path: &Path, when: std::time::SystemTime) {
        let file = File::options()
            .write(true)
            .open(path)
            .expect("openable for times update");
        let times = rustix::fs::Timestamps {
            last_access: to_timespec(when),
            last_modification: to_timespec(when),
        };
        rustix::fs::futimens(&file, &times).expect("futimens");
    }

    fn to_timespec(when: std::time::SystemTime) -> rustix::fs::Timespec {
        let delta = when
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .expect("post-epoch");
        rustix::fs::Timespec {
            tv_sec: i64::try_from(delta.as_secs()).expect("fits"),
            tv_nsec: i64::from(delta.subsec_nanos()),
        }
    }

    #[test]
    fn tightens_pre_existing_backups_on_first_write() {
        let dir = TempDir::new().expect("temp dir");
        let legacy = dir.path().join("stream.jsonl.20240102-030405.678");
        fs::write(&legacy, b"legacy\n").expect("seed");
        fs::set_permissions(&legacy, fs::Permissions::from_mode(0o644)).expect("chmod");

        let unrelated = dir.path().join("stream.jsonl.notabackup");
        fs::write(&unrelated, b"x\n").expect("seed");
        fs::set_permissions(&unrelated, fs::Permissions::from_mode(0o644)).expect("chmod");

        let writer = writer_in(&dir);
        writer.write_or_raise(&json!({"a": 1})).expect("write");

        assert_eq!(mode_of(&legacy), 0o600);
        assert_eq!(
            mode_of(&unrelated),
            0o644,
            "files that do not match the rotation pattern must be left alone"
        );
    }

    /// A backup-shaped symlink must not carry the tighten through to its target.
    ///
    /// Anyone who can create a file next to the log could otherwise point a
    /// backup-shaped name at an arbitrary file and have the writer chmod it to
    /// `0600`. Both versions defend the same way: stat without following, then
    /// re-check through an `O_NOFOLLOW` descriptor.
    #[test]
    fn a_backup_shaped_symlink_is_left_alone_together_with_its_target() {
        let dir = TempDir::new().expect("temp dir");
        let target = dir.path().join("unrelated.txt");
        fs::write(&target, b"unrelated\n").expect("seed");
        fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).expect("chmod");
        let link = dir.path().join("stream.jsonl.20260101-120000.100");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        writer_in(&dir)
            .write_or_raise(&json!({"a": 1}))
            .expect("write");

        assert!(
            fs::symlink_metadata(&link)
                .expect("link metadata")
                .file_type()
                .is_symlink(),
            "the link itself must survive untouched"
        );
        assert_eq!(mode_of(&target), 0o644);
    }

    /// A rotation that cannot rename reports, and the write still lands.
    ///
    /// This is the one failure v1 forwards to `on_error` without failing the
    /// call: the log keeps growing past its limit, which is exactly the state an
    /// operator needs to hear about. A read-only parent directory reproduces it
    /// faithfully — `O_APPEND` on the existing file still works, only the rename
    /// is denied.
    #[test]
    fn a_rotation_that_cannot_rename_reports_and_still_appends() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stream.jsonl");
        let errors = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&errors);
        let writer = JsonlEventWriter::new(&path)
            .with_max_bytes(1)
            .with_error_handler(Box::new(move |_| {
                counter.fetch_add(1, Ordering::SeqCst);
            }));

        writer.write(&json!({"a": 1}));
        assert_eq!(errors.load(Ordering::SeqCst), 0);

        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o500)).expect("chmod dir");
        writer.write(&json!({"a": 2}));
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("restore");

        assert_eq!(errors.load(Ordering::SeqCst), 1);
        let contents = fs::read_to_string(&path).expect("read log");
        assert_eq!(
            contents.lines().count(),
            2,
            "a failed rotation must not cost the caller its line"
        );
    }

    #[test]
    fn write_swallows_failures_and_notifies() {
        let dir = TempDir::new().expect("temp dir");
        // A directory in place of the log file makes every open fail.
        let path = dir.path().join("stream.jsonl");
        fs::create_dir(&path).expect("seed directory");

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let writer = JsonlEventWriter::new(&path).with_error_handler(Box::new(move |_| {
            counter.fetch_add(1, Ordering::SeqCst);
        }));

        writer.write(&json!({"a": 1}));
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let err = writer
            .write_or_raise(&json!({"a": 1}))
            .expect_err("write_or_raise must surface the failure");
        assert!(matches!(err, EventLogError::Io { .. }));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "write_or_raise must not invoke the swallow-path handler"
        );
    }

    /// A record whose serialization always fails, standing in for v1's
    /// `json.dumps` `TypeError` on unserializable payloads.
    struct Unserializable;

    impl Serialize for Unserializable {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("record is not serializable"))
        }
    }

    #[test]
    fn write_swallows_panicking_error_handlers() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stream.jsonl");
        fs::create_dir(&path).expect("seed directory");
        let writer = JsonlEventWriter::new(&path).with_error_handler(Box::new(|_| {
            panic!("error handler failure");
        }));

        assert!(
            std::panic::catch_unwind(AssertUnwindSafe(|| writer.write(&json!({"a": 1})))).is_ok()
        );
    }

    #[test]
    fn write_swallows_serialization_failures() {
        let dir = TempDir::new().expect("temp dir");
        let writer = writer_in(&dir);
        let err = writer
            .write_or_raise(&Unserializable)
            .expect_err("unserializable records must fail");
        assert!(matches!(err, EventLogError::Serialize(_)));

        writer.write(&Unserializable);
        assert!(
            !writer.path().exists(),
            "a serialization failure must not create the log file"
        );
    }

    #[test]
    fn concurrent_writers_do_not_lose_lines() {
        let dir = TempDir::new().expect("temp dir");
        let path = dir.path().join("stream.jsonl");
        let writer = Arc::new(JsonlEventWriter::new(&path));

        std::thread::scope(|scope| {
            for thread_id in 0..4 {
                let writer = Arc::clone(&writer);
                scope.spawn(move || {
                    for index in 0..25 {
                        writer
                            .write_or_raise(&json!({"t": thread_id, "i": index}))
                            .expect("write");
                    }
                });
            }
        });

        assert_eq!(read_lines(&path).len(), 100);
    }

    #[test]
    fn backup_suffix_matcher_mirrors_the_v1_regex() {
        for good in [
            "20240102-030405.678",
            "20240102-030405.678.1",
            "20240102-030405.678.999",
        ] {
            assert!(is_backup_suffix(good), "{good} should match");
        }
        for bad in [
            "",
            "notabackup",
            "2024010-030405.678",
            "20240102-03045.678",
            "20240102-030405.67",
            "20240102-030405.6789",
            "20240102-030405.678.",
            "20240102-030405.678.1.2",
            "20240102-030405.678.x",
            "20240102030405.678",
            "lock",
        ] {
            assert!(!is_backup_suffix(bad), "{bad} should not match");
        }
    }

    #[test]
    fn lock_path_is_the_log_name_plus_lock() {
        let writer = JsonlEventWriter::new("/tmp/a/stream.jsonl");
        assert_eq!(writer.lock_path(), Path::new("/tmp/a/stream.jsonl.lock"));
    }

    #[test]
    fn accessors_report_configuration() {
        let writer = JsonlEventWriter::new("/tmp/a/stream.jsonl")
            .with_max_bytes(7)
            .with_backup_count(2)
            .with_error_prefix("[x]");
        assert_eq!(writer.max_bytes(), 7);
        assert_eq!(writer.backup_count(), 2);
        assert_eq!(writer.error_prefix(), "[x]");
        assert!(format!("{writer:?}").contains("has_error_handler: false"));
    }
}
