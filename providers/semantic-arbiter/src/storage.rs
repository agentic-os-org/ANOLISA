//! Pinned private directories and conservative fixed reservations survive killed writers.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::time::Instant;

use crate::config::Config;
use crate::session::SessionTask;

pub(crate) const RESERVE_BYTES: u64 = 4 * 1024 * 1024;
/// Fixed reservation per captured session; a session file never exceeds it.
pub(crate) const SESSION_RESERVE: u64 = 128 * 1024;
const SESSIONS: &str = "sessions";
const FILE_LIMIT: usize = 1100 * 1024;
const FILES: &[&str] = &[
    "00-input.json",
    "05-task.json",
    "10-evaluation.json",
    "20-model-request.json",
    "30-model-result.json",
    "40-result.json",
];

fn denied() -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}

fn cstr(value: &str) -> io::Result<CString> {
    CString::new(value).map_err(|_| denied())
}

fn open_at(parent: &File, name: &str, flags: i32, mode: u32) -> io::Result<File> {
    let name = cstr(name)?;
    // SAFETY: valid NUL-terminated name and live directory descriptor; the
    // returned descriptor is owned exactly once by File on success.
    let fd = unsafe {
        libc::openat(
            parent.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            mode,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn private_dir(file: &File) -> io::Result<()> {
    let meta = file.metadata()?;
    // SAFETY: geteuid has no pointer arguments or preconditions.
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o7777 != 0o700 {
        return Err(denied());
    }
    Ok(())
}

pub(crate) fn open_root(path: &str) -> io::Result<File> {
    if !crate::config::absolute(path) || path == "/" {
        return Err(denied());
    }
    let mut dir = File::open("/")?;
    for part in path[1..].split('/') {
        dir = open_at(&dir, part, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    }
    private_dir(&dir)?;
    Ok(dir)
}

struct Lock<'a>(&'a File);
impl<'a> Lock<'a> {
    async fn acquire(file: &'a File, deadline: Instant) -> io::Result<Self> {
        loop {
            if Instant::now() >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            // SAFETY: flock only accesses the live descriptor.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Self(file));
            }
            let error = io::Error::last_os_error();
            if !matches!(
                error.kind(),
                io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
            ) {
                return Err(error);
            }
            tokio::time::sleep_until((Instant::now() + Duration::from_millis(1)).min(deadline))
                .await;
        }
    }
}
impl Drop for Lock<'_> {
    fn drop(&mut self) {
        // SAFETY: the borrowed file outlives this lock guard.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

fn entries(dir: &File) -> io::Result<fs::ReadDir> {
    // Linux procfs names an already-open directory, not a replaceable config path.
    fs::read_dir(format!("/proc/self/fd/{}", dir.as_raw_fd()))
}

fn open_record_entry(dir: &File, name: &str) -> io::Result<File> {
    open_at(dir, name, libc::O_RDONLY | libc::O_NONBLOCK, 0).or_else(|error| {
        match (error.kind(), name.strip_suffix(".pending")) {
            // Publication may unlink the pending name after directory enumeration.
            (io::ErrorKind::NotFound, Some(published)) => {
                open_at(dir, published, libc::O_RDONLY | libc::O_NONBLOCK, 0)
            }
            _ => Err(error),
        }
    })
}

fn quota() -> io::Error {
    io::Error::other("quota")
}

fn check_file(file: &File, limit: u64) -> io::Result<fs::Metadata> {
    let meta = file.metadata()?;
    if !meta.is_file() || meta.mode() & 0o7777 != 0o600 || meta.len() > limit {
        return Err(denied());
    }
    Ok(meta)
}

struct Usage {
    records: u64,
    sessions: u64,
}

impl Usage {
    fn bytes(&self) -> u64 {
        self.records
            .saturating_mul(RESERVE_BYTES)
            .saturating_add(self.sessions.saturating_mul(SESSION_RESERVE))
    }
}

/// Validates every existing entry; scanning stops once either count is over quota.
fn scan(root: &File, config: &Config) -> io::Result<Usage> {
    let mut usage = Usage {
        records: 0,
        sessions: 0,
    };
    for entry in entries(root)? {
        let name = entry?.file_name();
        let name = name.to_str().ok_or_else(denied)?;
        if name == SESSIONS {
            usage.sessions = scan_sessions(root, config)?;
            continue;
        }
        if !name.starts_with("record-") {
            return Err(denied());
        }
        usage.records += 1;
        if usage.records > config.max_records {
            return Err(quota());
        }
        check_record(root, name)?;
    }
    Ok(usage)
}

fn check_record(root: &File, name: &str) -> io::Result<()> {
    let dir = open_at(root, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    private_dir(&dir)?;
    let mut bytes = 0u64;
    let mut files = BTreeSet::new();
    for (index, child) in entries(&dir)?.enumerate() {
        if index >= FILES.len() * 2 {
            return Err(denied());
        }
        let name = child?.file_name();
        let name = name.to_str().ok_or_else(denied)?;
        let base = name.strip_suffix(".pending").unwrap_or(name);
        if !FILES.contains(&base) {
            return Err(denied());
        }
        let meta = check_file(&open_record_entry(&dir, name)?, FILE_LIMIT as u64)?;
        if !files.insert((meta.dev(), meta.ino())) {
            continue;
        }
        bytes = bytes.saturating_add(meta.len());
        if bytes > RESERVE_BYTES {
            return Err(denied());
        }
    }
    Ok(())
}

/// Session files are only written under the root lock, so no entry vanishes mid-scan.
fn scan_sessions(root: &File, config: &Config) -> io::Result<u64> {
    let dir = open_at(root, SESSIONS, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    private_dir(&dir)?;
    let mut keys = BTreeSet::new();
    for entry in entries(&dir)? {
        let name = entry?.file_name();
        let name = name.to_str().ok_or_else(denied)?;
        let key = name.strip_suffix(".pending").unwrap_or(name);
        if !session_key(key) {
            return Err(denied());
        }
        check_file(
            &open_at(&dir, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?,
            SESSION_RESERVE,
        )?;
        keys.insert(key.to_owned());
        if keys.len() as u64 > config.max_records {
            return Err(quota());
        }
    }
    Ok(keys.len() as u64)
}

fn session_key(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn session_name(session_id: &str) -> io::Result<&str> {
    session_id
        .strip_prefix("sha256:")
        .filter(|key| session_key(key))
        .ok_or_else(denied)
}

fn read_bounded(mut file: File, limit: u64) -> io::Result<Vec<u8>> {
    check_file(&file, limit)?;
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut io::Read::take(&mut file, limit + 1), &mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(denied());
    }
    Ok(bytes)
}

fn missing(error: io::Error) -> io::Result<Option<File>> {
    if error.kind() == io::ErrorKind::NotFound {
        Ok(None)
    } else {
        Err(error)
    }
}

fn open_sessions(root: &File) -> io::Result<Option<File>> {
    match open_at(root, SESSIONS, libc::O_RDONLY | libc::O_DIRECTORY, 0) {
        Ok(dir) => {
            private_dir(&dir)?;
            Ok(Some(dir))
        }
        Err(error) => missing(error),
    }
}

fn load_session(dir: &File, name: &str) -> io::Result<Option<SessionTask>> {
    let file = match open_at(dir, name, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(error) => return missing(error).map(|_| None),
    };
    let bytes = read_bounded(file, SESSION_RESERVE)?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| denied())
}

fn storage_code(error: io::Error) -> &'static str {
    if error.kind() == io::ErrorKind::TimedOut {
        "budget_exceeded"
    } else {
        "record_unavailable"
    }
}

/// Applies `update` to one session's stored task under the root lock.
///
/// The replacement is published by rename, so readers without the lock see
/// either the previous or the new complete file. `update` returns whether it
/// changed the task; an unchanged task is not rewritten.
///
/// # Errors
///
/// `budget_exceeded` when the lock or write cannot finish before `deadline`;
/// `record_unavailable` for quota exhaustion or any unsafe directory state.
pub(crate) async fn update_session(
    config: &Config,
    session_id: &str,
    deadline: Instant,
    update: impl FnOnce(&mut SessionTask) -> bool,
) -> Result<(), &'static str> {
    update_session_io(config, session_id, deadline, update)
        .await
        .map_err(storage_code)
}

async fn update_session_io(
    config: &Config,
    session_id: &str,
    deadline: Instant,
    update: impl FnOnce(&mut SessionTask) -> bool,
) -> io::Result<()> {
    let name = session_name(session_id)?;
    let root = open_root(&config.record_dir)?;
    let _lock = Lock::acquire(&root, deadline).await?;
    let usage = scan(&root, config)?;
    let dir = match open_sessions(&root)? {
        Some(dir) => dir,
        None => {
            let c_name = cstr(SESSIONS)?;
            // SAFETY: live root descriptor and NUL-terminated relative basename.
            if unsafe { libc::mkdirat(root.as_raw_fd(), c_name.as_ptr(), 0o700) } != 0 {
                return Err(io::Error::last_os_error());
            }
            root.sync_all()?;
            open_sessions(&root)?.ok_or_else(denied)?
        }
    };
    let existing = load_session(&dir, name)?;
    if existing.is_none()
        && (usage.sessions + 1 > config.max_records
            || usage.bytes().saturating_add(SESSION_RESERVE) > config.max_storage_bytes)
    {
        return Err(quota());
    }
    let mut task = existing.unwrap_or_default();
    if !update(&mut task) {
        return Ok(());
    }
    let bytes = serde_json::to_vec(&task)?;
    if bytes.len() as u64 > SESSION_RESERVE {
        return Err(quota());
    }
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let temp = format!("{name}.pending");
    let from = cstr(&temp)?;
    // SAFETY: the lock excludes other writers, so a pending name is a killed
    // writer's leftover inside this pinned directory.
    if unsafe { libc::unlinkat(dir.as_raw_fd(), from.as_ptr(), 0) } != 0 {
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::NotFound {
            return Err(error);
        }
    }
    let mut file = open_at(
        &dir,
        &temp,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )?;
    check_file(&file, SESSION_RESERVE)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    let to = cstr(name)?;
    // SAFETY: both basenames refer to this pinned directory; replacing the
    // previous session file is the intended publication.
    if unsafe { libc::renameat(dir.as_raw_fd(), from.as_ptr(), dir.as_raw_fd(), to.as_ptr()) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    dir.sync_all()
}

pub(crate) struct Record {
    // Keep the root pinned so the session task is read from the same tree.
    root: File,
    dir: File,
    used: Mutex<u64>,
}

impl Record {
    pub(crate) async fn create(config: &Config, deadline: Instant) -> Result<Self, &'static str> {
        Self::create_io(config, deadline)
            .await
            .map_err(storage_code)
    }

    async fn create_io(config: &Config, deadline: Instant) -> io::Result<Self> {
        let root = open_root(&config.record_dir)?;
        let lock = Lock::acquire(&root, deadline).await?;
        let usage = scan(&root, config)?;
        if usage.records + 1 > config.max_records
            || usage.bytes().saturating_add(RESERVE_BYTES) > config.max_storage_bytes
        {
            return Err(quota());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| denied())?
            .as_nanos();
        let mut created = None;
        for nonce in 0..8 {
            let name = format!("record-{timestamp}-{}-{nonce}", std::process::id());
            let c_name = cstr(&name)?;
            // SAFETY: live root descriptor and NUL-terminated relative basename.
            if unsafe { libc::mkdirat(root.as_raw_fd(), c_name.as_ptr(), 0o700) } == 0 {
                created = Some(name);
                break;
            }
            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
        }
        let dir = open_at(
            &root,
            &created.ok_or_else(denied)?,
            libc::O_RDONLY | libc::O_DIRECTORY,
            0,
        )?;
        private_dir(&dir)?;
        root.sync_all()?;
        drop(lock);
        Ok(Self {
            root,
            dir,
            used: Mutex::new(0),
        })
    }

    /// Reads the captured task of one session from the pinned root.
    ///
    /// # Errors
    ///
    /// `task_unavailable` when no prompt was captured for the session;
    /// `record_unavailable` when the stored file is unsafe or malformed.
    pub(crate) fn session_task(&self, session_id: &str) -> Result<SessionTask, &'static str> {
        let load = || -> io::Result<Option<SessionTask>> {
            let name = session_name(session_id)?;
            match open_sessions(&self.root)? {
                Some(dir) => load_session(&dir, name),
                None => Ok(None),
            }
        };
        match load() {
            Ok(Some(task)) => Ok(task),
            Ok(None) => Err("task_unavailable"),
            Err(_) => Err("record_unavailable"),
        }
    }

    pub(crate) fn save(&self, name: &str, value: &impl Serialize) -> Result<(), &'static str> {
        self.save_io(name, value).map_err(|_| "record_write_failed")
    }

    fn save_io(&self, name: &str, value: &impl Serialize) -> io::Result<()> {
        if !FILES.contains(&name) {
            return Err(denied());
        }
        let bytes = serde_json::to_vec(value)?;
        let mut used = self.used.lock().map_err(|_| denied())?;
        if bytes.len() > FILE_LIMIT || used.saturating_add(bytes.len() as u64) > RESERVE_BYTES {
            return Err(io::Error::other("quota"));
        }
        let temp = format!("{name}.pending");
        let mut file = open_at(
            &self.dir,
            &temp,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )?;
        if file.metadata()?.mode() & 0o7777 != 0o600 {
            return Err(denied());
        }
        // Count even partially failed writes; no retries, overwrites or cleanup.
        *used += bytes.len() as u64;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let from = cstr(&temp)?;
        let to = cstr(name)?;
        // SAFETY: both basenames refer to this pinned directory. linkat fails
        // if the destination exists, unlike rename's overwriting publication.
        if unsafe {
            libc::linkat(
                self.dir.as_raw_fd(),
                from.as_ptr(),
                self.dir.as_raw_fd(),
                to.as_ptr(),
                0,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: only remove the temporary link owned by this publication.
        if unsafe { libc::unlinkat(self.dir.as_raw_fd(), from.as_ptr(), 0) } != 0 {
            return Err(io::Error::last_os_error());
        }
        self.dir.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerated_pending_entry_survives_atomic_publication() {
        let temp = tempfile::tempdir().unwrap();
        let dir = File::open(temp.path()).unwrap();
        let pending = "00-input.json.pending";
        open_at(
            &dir,
            pending,
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
        .unwrap()
        .write_all(b"{}")
        .unwrap();
        let enumerated = entries(&dir).unwrap().next().unwrap().unwrap().file_name();
        fs::hard_link(temp.path().join(pending), temp.path().join("00-input.json")).unwrap();
        fs::remove_file(temp.path().join(pending)).unwrap();
        assert_eq!(
            open_record_entry(&dir, enumerated.to_str().unwrap())
                .unwrap()
                .metadata()
                .unwrap()
                .len(),
            2
        );
        assert!(open_record_entry(&dir, "10-evaluation.json.pending").is_err());
        std::os::unix::fs::symlink(
            temp.path().join("00-input.json"),
            temp.path().join("10-evaluation.json"),
        )
        .unwrap();
        assert!(open_record_entry(&dir, "10-evaluation.json.pending").is_err());
    }
}
