//! Pinned private directories and conservative fixed reservations survive killed writers.

use std::collections::BTreeSet;
use std::ffi::CString;
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::MetadataExt;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::config::Config;

pub(crate) const RESERVE_BYTES: u64 = 4 * 1024 * 1024;
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
    fn acquire(file: &'a File) -> io::Result<Self> {
        // SAFETY: flock only accesses the live descriptor. Nonblocking avoids
        // consuming the inference budget waiting for another reservation.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(file))
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

fn check_existing(root: &File, config: &Config) -> io::Result<()> {
    let mut count = 0u64;
    for entry in entries(root)? {
        count += 1;
        if count >= config.max_records || (count + 1) * RESERVE_BYTES > config.max_storage_bytes {
            return Err(io::Error::other("quota"));
        }
        let name = entry?.file_name();
        let name = name.to_str().ok_or_else(denied)?;
        if !name.starts_with("record-") {
            return Err(denied());
        }
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
            let file = open_record_entry(&dir, name)?;
            let meta = file.metadata()?;
            if !meta.is_file() || meta.mode() & 0o7777 != 0o600 || meta.len() > FILE_LIMIT as u64 {
                return Err(denied());
            }
            if !files.insert((meta.dev(), meta.ino())) {
                continue;
            }
            bytes = bytes.saturating_add(meta.len());
            if bytes > RESERVE_BYTES {
                return Err(denied());
            }
        }
    }
    Ok(())
}

pub(crate) struct Record {
    // Keep the root pinned for the lifetime of this invocation as well.
    _root: File,
    dir: File,
    used: Mutex<u64>,
}

impl Record {
    pub(crate) fn create(config: &Config) -> Result<Self, &'static str> {
        Self::create_io(config).map_err(|_| "record_unavailable")
    }

    fn create_io(config: &Config) -> io::Result<Self> {
        let root = open_root(&config.record_dir)?;
        let lock = Lock::acquire(&root)?;
        check_existing(&root, config)?;
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
            _root: root,
            dir,
            used: Mutex::new(0),
        })
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
