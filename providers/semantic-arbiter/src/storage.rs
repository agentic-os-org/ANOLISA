//! Pinned private per-session directories with append-only call logs.
//!
//! The root holds one `session-<hex>` directory per scoped session (its
//! captured `task.json` and its `calls.jsonl`) plus `unbound.jsonl` for calls
//! whose session cannot be identified. Every write happens under the root
//! `flock`, so quotas are checked against actual bytes and lines never interleave.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileExt, MetadataExt};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use tokio::time::Instant;

use crate::config::Config;
use crate::session::SessionTask;

/// Bytes one call may append; a call is admitted only with this much headroom.
pub(crate) const CALL_LIMIT: u64 = 4 * 1024 * 1024;
/// Upper bound of one session's `task.json`.
pub(crate) const TASK_LIMIT: u64 = 128 * 1024;
const LINE_LIMIT: usize = 1100 * 1024;
const SESSION_PREFIX: &str = "session-";
const TASK: &str = "task.json";
const TASK_PENDING: &str = "task.json.pending";
const CALLS: &str = "calls.jsonl";
const UNBOUND: &str = "unbound.jsonl";
const STAGES: &[&str] = &[
    "input",
    "task",
    "evaluation",
    "model_request",
    "model_result",
    "result",
];

fn denied() -> io::Error {
    io::Error::from(io::ErrorKind::PermissionDenied)
}

fn quota() -> io::Error {
    io::Error::other("quota")
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

fn open_dir(parent: &File, name: &str) -> io::Result<File> {
    let dir = open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    private_dir(&dir)?;
    Ok(dir)
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
    /// Always tries once, even past `deadline`, so a late stage of an admitted
    /// call is still recorded when nobody else holds the lock.
    fn acquire(file: &'a File, deadline: Instant) -> io::Result<Self> {
        loop {
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
            let now = Instant::now();
            if now >= deadline {
                return Err(io::ErrorKind::TimedOut.into());
            }
            std::thread::sleep((deadline - now).min(Duration::from_millis(1)));
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

fn check_file(file: &File) -> io::Result<fs::Metadata> {
    let meta = file.metadata()?;
    if !meta.is_file() || meta.mode() & 0o7777 != 0o600 {
        return Err(denied());
    }
    Ok(meta)
}

fn size_at(dir: &File, name: &str) -> io::Result<u64> {
    match open_at(dir, name, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => Ok(check_file(&file)?.len()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

#[derive(Clone, Copy)]
struct Limits {
    sessions: u64,
    session_bytes: u64,
    storage_bytes: u64,
}

impl Limits {
    fn of(config: &Config) -> Self {
        Self {
            sessions: config.max_sessions,
            session_bytes: config.max_session_bytes,
            storage_bytes: config.max_storage_bytes,
        }
    }
}

struct Usage {
    sessions: u64,
    total: u64,
    /// Bytes of the requested root entry, or `None` when it does not exist.
    sink: Option<u64>,
}

/// Validates every root entry and sums actual bytes; called only under the lock,
/// so no entry is created, renamed or grown while it is enumerated.
fn scan(root: &File, limits: Limits, sink: &str) -> io::Result<Usage> {
    let mut usage = Usage {
        sessions: 0,
        total: 0,
        sink: None,
    };
    for entry in entries(root)? {
        let name = entry?.file_name();
        let name = name.to_str().ok_or_else(denied)?;
        let bytes = if name == UNBOUND {
            check_file(&open_at(root, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?)?.len()
        } else if name.strip_prefix(SESSION_PREFIX).is_some_and(session_key) {
            usage.sessions += 1;
            if usage.sessions > limits.sessions {
                return Err(quota());
            }
            session_bytes(&open_dir(root, name)?)?
        } else {
            return Err(denied());
        };
        usage.total = usage.total.saturating_add(bytes);
        if name == sink {
            usage.sink = Some(bytes);
        }
    }
    Ok(usage)
}

fn session_bytes(dir: &File) -> io::Result<u64> {
    let mut bytes = 0u64;
    for (index, child) in entries(dir)?.enumerate() {
        let name = child?.file_name();
        let name = name.to_str().ok_or_else(denied)?;
        if index >= 3 || ![TASK, TASK_PENDING, CALLS].contains(&name) {
            return Err(denied());
        }
        let size = check_file(&open_at(dir, name, libc::O_RDONLY | libc::O_NONBLOCK, 0)?)?.len();
        if name != CALLS && size > TASK_LIMIT {
            return Err(denied());
        }
        bytes = bytes.saturating_add(size);
    }
    Ok(bytes)
}

fn session_key(name: &str) -> bool {
    name.len() == 64
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn session_dir(session_id: &str) -> io::Result<String> {
    session_id
        .strip_prefix("sha256:")
        .filter(|key| session_key(key))
        .map(|key| format!("{SESSION_PREFIX}{key}"))
        .ok_or_else(denied)
}

fn existing_dir(root: &File, name: &str) -> io::Result<Option<File>> {
    match open_dir(root, name) {
        Ok(dir) => Ok(Some(dir)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn make_dir(root: &File, name: &str) -> io::Result<File> {
    let c_name = cstr(name)?;
    // SAFETY: live root descriptor and NUL-terminated relative basename.
    if unsafe { libc::mkdirat(root.as_raw_fd(), c_name.as_ptr(), 0o700) } != 0 {
        return Err(io::Error::last_os_error());
    }
    root.sync_all()?;
    open_dir(root, name)
}

fn load_task(dir: &File) -> io::Result<Option<SessionTask>> {
    let mut file = match open_at(dir, TASK, libc::O_RDONLY | libc::O_NONBLOCK, 0) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    check_file(&file)?;
    let mut bytes = Vec::new();
    io::Read::read_to_end(&mut io::Read::take(&mut file, TASK_LIMIT + 1), &mut bytes)?;
    if bytes.len() as u64 > TASK_LIMIT {
        return Err(denied());
    }
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

/// Applies `update` to one session's `task.json` under the root lock.
///
/// The replacement is published by rename, so readers without the lock see
/// either the previous or the new complete file. `update` returns whether it
/// changed the task; an unchanged task is not rewritten.
///
/// # Errors
///
/// `budget_exceeded` when the lock or write cannot finish before `deadline`;
/// `record_unavailable` for quota exhaustion or any unsafe directory state.
pub(crate) fn update_session(
    config: &Config,
    session_id: &str,
    deadline: Instant,
    update: impl FnOnce(&mut SessionTask) -> bool,
) -> Result<(), &'static str> {
    update_session_io(config, session_id, deadline, update).map_err(storage_code)
}

fn update_session_io(
    config: &Config,
    session_id: &str,
    deadline: Instant,
    update: impl FnOnce(&mut SessionTask) -> bool,
) -> io::Result<()> {
    let limits = Limits::of(config);
    let name = session_dir(session_id)?;
    let root = open_root(&config.record_dir)?;
    let _lock = Lock::acquire(&root, deadline)?;
    let usage = scan(&root, limits, &name)?;
    let dir = existing_dir(&root, &name)?;
    let (mut task, old) = match &dir {
        Some(dir) => (
            load_task(dir)?.unwrap_or_default(),
            size_at(dir, TASK)?.saturating_add(size_at(dir, TASK_PENDING)?),
        ),
        None => (SessionTask::default(), 0),
    };
    if !update(&mut task) {
        return Ok(());
    }
    let bytes = serde_json::to_vec(&task)?;
    let grown = |used: u64| used.saturating_sub(old).saturating_add(bytes.len() as u64);
    if bytes.len() as u64 > TASK_LIMIT
        || (dir.is_none() && usage.sessions >= limits.sessions)
        || grown(usage.sink.unwrap_or(0)) > limits.session_bytes
        || grown(usage.total) > limits.storage_bytes
    {
        return Err(quota());
    }
    if Instant::now() >= deadline {
        return Err(io::ErrorKind::TimedOut.into());
    }
    let dir = match dir {
        Some(dir) => dir,
        None => make_dir(&root, &name)?,
    };
    let from = cstr(TASK_PENDING)?;
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
        TASK_PENDING,
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )?;
    check_file(&file)?;
    file.write_all(&bytes)?;
    file.sync_all()?;
    let to = cstr(TASK)?;
    // SAFETY: both basenames refer to this pinned directory; replacing the
    // previous task file is the intended publication.
    if unsafe { libc::renameat(dir.as_raw_fd(), from.as_ptr(), dir.as_raw_fd(), to.as_ptr()) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    dir.sync_all()
}

#[derive(Serialize)]
struct Line<'a, T: Serialize> {
    call: &'a str,
    stage: &'a str,
    data: &'a T,
}

#[derive(Default)]
struct Written {
    bytes: u64,
    stages: BTreeSet<&'static str>,
}

/// One `tool.before` call appending its stages to its session or unbound log.
pub(crate) struct Record {
    // Keep the root pinned so quota scans and the session task use the same tree.
    root: File,
    /// Pinned session directory; `None` writes to the root's unbound log.
    session: Option<File>,
    sink: String,
    call: String,
    limits: Limits,
    deadline: Instant,
    written: Mutex<Written>,
}

impl Record {
    /// Admits one call when its sink and the root still have a full call of headroom.
    ///
    /// Headroom is not a reservation: concurrent calls can all be admitted, and
    /// the hard caps are then enforced per line by [`Record::save`].
    ///
    /// # Errors
    ///
    /// `budget_exceeded` when the lock cannot be taken before `deadline`;
    /// `record_unavailable` for quota exhaustion or any unsafe directory state.
    pub(crate) fn create(
        config: &Config,
        session_id: Option<&str>,
        deadline: Instant,
    ) -> Result<Self, &'static str> {
        Self::create_io(config, session_id, deadline).map_err(storage_code)
    }

    fn create_io(config: &Config, session_id: Option<&str>, deadline: Instant) -> io::Result<Self> {
        let limits = Limits::of(config);
        let sink = match session_id {
            Some(session_id) => session_dir(session_id)?,
            None => UNBOUND.to_owned(),
        };
        let root = open_root(&config.record_dir)?;
        let lock = Lock::acquire(&root, deadline)?;
        let usage = scan(&root, limits, &sink)?;
        let new_session = session_id.is_some() && usage.sink.is_none();
        if (new_session && usage.sessions >= limits.sessions)
            || usage.sink.unwrap_or(0).saturating_add(CALL_LIMIT) > limits.session_bytes
            || usage.total.saturating_add(CALL_LIMIT) > limits.storage_bytes
        {
            return Err(quota());
        }
        if Instant::now() >= deadline {
            return Err(io::ErrorKind::TimedOut.into());
        }
        let session = match (session_id, new_session) {
            (None, _) => None,
            (Some(_), true) => Some(make_dir(&root, &sink)?),
            (Some(_), false) => Some(open_dir(&root, &sink)?),
        };
        drop(lock);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| denied())?
            .as_nanos();
        Ok(Self {
            root,
            session,
            sink,
            call: format!("{timestamp}-{}", std::process::id()),
            limits,
            deadline,
            written: Mutex::new(Written::default()),
        })
    }

    /// Reads the captured task of this call's session from the pinned directory.
    ///
    /// # Errors
    ///
    /// `task_unavailable` when no prompt was captured for the session;
    /// `record_unavailable` when the stored file is unsafe or malformed.
    pub(crate) fn session_task(&self) -> Result<SessionTask, &'static str> {
        let Some(dir) = &self.session else {
            return Err("task_unavailable");
        };
        match load_task(dir) {
            Ok(Some(task)) => Ok(task),
            Ok(None) => Err("task_unavailable"),
            Err(_) => Err("record_unavailable"),
        }
    }

    /// Appends one stage line; each stage is written at most once per call.
    pub(crate) fn save(
        &self,
        stage: &'static str,
        value: &impl Serialize,
    ) -> Result<(), &'static str> {
        self.save_io(stage, value)
            .map_err(|_| "record_write_failed")
    }

    fn save_io(&self, stage: &'static str, value: &impl Serialize) -> io::Result<()> {
        if !STAGES.contains(&stage) {
            return Err(denied());
        }
        let mut line = serde_json::to_vec(&Line {
            call: &self.call,
            stage,
            data: value,
        })?;
        line.push(b'\n');
        let mut written = self.written.lock().map_err(|_| denied())?;
        if line.len() > LINE_LIMIT
            || written.stages.contains(stage)
            || written.bytes.saturating_add(line.len() as u64) > CALL_LIMIT
        {
            return Err(quota());
        }
        let _lock = Lock::acquire(&self.root, self.deadline)?;
        let usage = scan(&self.root, self.limits, &self.sink)?;
        let parent = self.session.as_ref().unwrap_or(&self.root);
        let (name, sink) = match self.session {
            // A removed session directory must not be silently recreated elsewhere.
            Some(_) => (CALLS, usage.sink.ok_or_else(denied)?),
            None => (UNBOUND, usage.sink.unwrap_or(0)),
        };
        let file = open_at(
            parent,
            name,
            libc::O_RDWR | libc::O_APPEND | libc::O_CREAT,
            0o600,
        )?;
        let len = check_file(&file)?.len();
        // A writer killed mid-line leaves a torn tail; start on a fresh line.
        let mut last = [b'\n'];
        if len > 0 {
            file.read_exact_at(&mut last, len - 1)?;
        }
        if last[0] != b'\n' {
            line.insert(0, b'\n');
        }
        let size = line.len() as u64;
        if sink.saturating_add(size) > self.limits.session_bytes
            || usage.total.saturating_add(size) > self.limits.storage_bytes
        {
            return Err(quota());
        }
        // Count even partially failed writes; no retries, rewrites or cleanup.
        written.bytes += size;
        written.stages.insert(stage);
        (&file).write_all(&line)?;
        file.sync_all()?;
        if len == 0 {
            parent.sync_all()?;
        }
        Ok(())
    }
}
