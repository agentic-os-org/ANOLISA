//! Descriptor-relative access to the workspace-scoped session directory.

use std::fs::{File, Permissions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use rustix::fs::{mkdirat, open, openat, renameat, unlinkat, AtFlags, Dir, Mode, OFlags, RawMode};

use super::io::{io_error, now_ms, open_file_time_ms, try_exclusive_lock, SessionLock};
use super::listing::ListEntry;
use super::{ProviderSessionId, SessionError};

const PRIVATE_DIRECTORY_MODE: u32 = 0o700;
const PRIVATE_FILE_MODE: u32 = 0o600;
const STALE_TEMPORARY_FILE_MS: u64 = 60 * 60 * 1000;

/// Builds a `Mode` portably: `RawMode` is `u32` on the linux_raw backend but
/// `mode_t` (`u16`) on the libc backend, so the permission bits need a cast.
// The cast is a no-op on linux_raw yet required for libc-backed targets.
#[allow(clippy::unnecessary_cast)]
fn permission_mode(bits: u32) -> Mode {
    Mode::from_bits_truncate(bits as RawMode)
}

/// Pins scoped storage and keeps every file operation relative to its descriptor.
pub(super) struct ScopedStorage {
    path: PathBuf,
    pinned: Mutex<PinnedDirectory>,
}

/// Pinned descriptor plus the once-per-store temporary-file sweep marker.
#[derive(Default)]
struct PinnedDirectory {
    directory: Option<File>,
    swept: bool,
}

impl ScopedStorage {
    /// Opens an existing scoped directory without following path components.
    pub(super) fn new(path: PathBuf) -> Result<Self, SessionError> {
        let directory = open_directory_path(&path, false)
            .map_err(|error| io_error("open scoped session directory", &path, error))?;
        Ok(Self {
            path,
            pinned: Mutex::new(PinnedDirectory {
                directory,
                swept: false,
            }),
        })
    }

    /// Builds a diagnostic pathname only; storage operations never dereference it.
    pub(super) fn session_path(&self, session_id: &ProviderSessionId) -> PathBuf {
        self.path.join(session_filename(session_id))
    }

    /// Opens the pinned directory, optionally creating missing components safely.
    pub(super) fn directory(&self, create: bool) -> Result<Option<File>, SessionError> {
        let mut pinned = self
            .pinned
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // An externally removed directory leaves a descriptor to an unlinked
        // inode; drop it so the next open observes the recreated directory.
        if pinned
            .directory
            .as_ref()
            .is_some_and(|directory| !directory_matches_path(directory, &self.path))
        {
            pinned.directory = None;
        }
        if pinned.directory.is_none() {
            pinned.directory = open_directory_path(&self.path, create)
                .map_err(|error| io_error("open scoped session directory", &self.path, error))?;
        }
        if create {
            let sweep = !pinned.swept && pinned.directory.is_some();
            if let Some(directory) = pinned.directory.as_ref() {
                directory
                    .set_permissions(Permissions::from_mode(PRIVATE_DIRECTORY_MODE))
                    .map_err(|error| {
                        io_error("set private directory permissions", &self.path, error)
                    })?;
                if sweep {
                    self.sweep_stale_temporary_files(directory);
                }
            }
            if sweep {
                pinned.swept = true;
            }
        }
        pinned
            .directory
            .as_ref()
            .map(File::try_clone)
            .transpose()
            .map_err(|error| io_error("clone scoped session directory", &self.path, error))
    }

    /// Best-effort removal of temporary files left behind by crashed writers.
    fn sweep_stale_temporary_files(&self, directory: &File) {
        let Ok(entries) = Dir::read_from(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(filename) = std::str::from_utf8(entry.file_name().to_bytes()) else {
                continue;
            };
            if !filename.starts_with('.') || !filename.ends_with(".tmp") {
                continue;
            }
            let Ok(descriptor) = openat(
                directory,
                filename,
                OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
                Mode::empty(),
            ) else {
                continue;
            };
            let file = File::from(descriptor);
            let modified_at_ms = open_file_time_ms(&file);
            if now_ms().saturating_sub(modified_at_ms) >= STALE_TEMPORARY_FILE_MS {
                let _ = unlinkat(directory, filename, AtFlags::empty());
            }
        }
    }

    /// Opens a regular session file relative to a pinned directory.
    pub(super) fn open_session(
        &self,
        directory: &File,
        session_id: &ProviderSessionId,
    ) -> Result<Option<File>, SessionError> {
        let filename = session_filename(session_id);
        let path = self.path.join(&filename);
        let descriptor = match openat(
            directory,
            filename.as_str(),
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) => return Ok(None),
            Err(rustix::io::Errno::LOOP) => {
                return Err(SessionError::Corrupt {
                    session_id: session_id.to_string(),
                    message: "scoped session entry is a symbolic link".to_string(),
                });
            }
            Err(error) => {
                return Err(io_error(
                    "open scoped session",
                    &path,
                    rustix_error_to_io(error),
                ));
            }
        };
        let file = File::from(descriptor);
        let metadata = file
            .metadata()
            .map_err(|error| io_error("inspect scoped session", &path, error))?;
        if !metadata.is_file() {
            return Err(SessionError::Corrupt {
                session_id: session_id.to_string(),
                message: "scoped session entry is not a regular file".to_string(),
            });
        }
        Ok(Some(file))
    }

    /// Lists regular canonical session entries from the pinned directory.
    pub(super) fn entries(&self, directory: &File) -> Result<Vec<ListEntry>, SessionError> {
        let entries = Dir::read_from(directory).map_err(|error| {
            io_error(
                "list scoped sessions",
                &self.path,
                rustix_error_to_io(error),
            )
        })?;
        let mut sessions = Vec::new();
        for entry in entries.flatten() {
            let Ok(filename) = std::str::from_utf8(entry.file_name().to_bytes()) else {
                continue;
            };
            let Some(stem) = filename.strip_suffix(".json") else {
                continue;
            };
            let Ok(session_id) = ProviderSessionId::parse(stem) else {
                continue;
            };
            let Ok(Some(file)) = self.open_session(directory, &session_id) else {
                continue;
            };
            sessions.push(ListEntry {
                modified_at_ms: open_file_time_ms(&file),
                session_id,
            });
        }
        Ok(sessions)
    }

    /// Acquires the advisory generation lock relative to the pinned directory.
    pub(super) fn acquire_lock(
        &self,
        directory: &File,
        session_id: &ProviderSessionId,
    ) -> Result<SessionLock, SessionError> {
        let filename = format!(".{}.lock", session_id.as_str());
        let path = self.path.join(&filename);
        let descriptor = openat(
            directory,
            filename.as_str(),
            OFlags::RDWR | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            permission_mode(PRIVATE_FILE_MODE),
        )
        .map_err(|error| io_error("open lock", &path, rustix_error_to_io(error)))?;
        let file = File::from(descriptor);
        file.set_permissions(Permissions::from_mode(PRIVATE_FILE_MODE))
            .map_err(|error| io_error("set private file permissions", &path, error))?;
        match try_exclusive_lock(&file) {
            Ok(true) => Ok(SessionLock { file }),
            Ok(false) => Err(SessionError::Conflict {
                session_id: session_id.to_string(),
            }),
            Err(error) => Err(io_error("lock", &path, error)),
        }
    }

    /// Atomically replaces one session through a same-directory temporary file.
    pub(super) fn write_atomic(
        &self,
        directory: &File,
        session_id: &ProviderSessionId,
        bytes: &[u8],
    ) -> Result<(), SessionError> {
        let destination = session_filename(session_id);
        let temporary = format!(".{session_id}.{}.tmp", uuid::Uuid::new_v4());
        let temp_path = self.path.join(&temporary);
        let destination_path = self.path.join(&destination);
        let result = (|| {
            let descriptor = openat(
                directory,
                temporary.as_str(),
                OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                permission_mode(PRIVATE_FILE_MODE),
            )
            .map_err(|error| {
                io_error(
                    "create temporary file",
                    &temp_path,
                    rustix_error_to_io(error),
                )
            })?;
            let mut file = File::from(descriptor);
            file.set_permissions(Permissions::from_mode(PRIVATE_FILE_MODE))
                .map_err(|error| io_error("set private file permissions", &temp_path, error))?;
            file.write_all(bytes)
                .map_err(|error| io_error("write temporary file", &temp_path, error))?;
            file.sync_all()
                .map_err(|error| io_error("sync temporary file", &temp_path, error))?;
            renameat(
                directory,
                temporary.as_str(),
                directory,
                destination.as_str(),
            )
            .map_err(|error| io_error("replace", &destination_path, rustix_error_to_io(error)))?;
            directory
                .sync_all()
                .map_err(|error| io_error("sync directory", &self.path, error))
        })();
        if result.is_err() {
            let _ = unlinkat(directory, temporary.as_str(), AtFlags::empty());
        }
        result
    }

    /// Removes a session filename relative to the pinned directory.
    ///
    /// The caller must hold the session's advisory lock; the paired lock file
    /// is removed afterwards so cleared sessions leave no residue behind.
    pub(super) fn remove_session(
        &self,
        directory: &File,
        session_id: &ProviderSessionId,
    ) -> Result<(), SessionError> {
        let filename = session_filename(session_id);
        unlinkat(directory, filename.as_str(), AtFlags::empty()).map_err(|error| {
            if error == rustix::io::Errno::NOENT {
                SessionError::NotFound {
                    session_id: session_id.to_string(),
                }
            } else {
                io_error(
                    "remove",
                    &self.path.join(filename),
                    rustix_error_to_io(error),
                )
            }
        })?;
        let lock_filename = format!(".{}.lock", session_id.as_str());
        let _ = unlinkat(directory, lock_filename.as_str(), AtFlags::empty());
        Ok(())
    }
}

fn directory_matches_path(directory: &File, path: &Path) -> bool {
    let Ok(pinned) = directory.metadata() else {
        return false;
    };
    let Ok(current) = std::fs::metadata(path) else {
        return false;
    };
    pinned.nlink() != 0 && pinned.dev() == current.dev() && pinned.ino() == current.ino()
}

fn session_filename(session_id: &ProviderSessionId) -> String {
    format!("{}.json", session_id.as_str())
}

fn open_directory_path(path: &Path, create: bool) -> io::Result<Option<File>> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "scoped session directory path is not absolute",
        ));
    }
    let descriptor = open(
        "/",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map_err(rustix_error_to_io)?;
    let mut directory = File::from(descriptor);
    for component in normalized_components(path)? {
        let opened = openat(
            &directory,
            component.as_os_str(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        );
        let descriptor = match opened {
            Ok(descriptor) => descriptor,
            Err(rustix::io::Errno::NOENT) if !create => return Ok(None),
            Err(rustix::io::Errno::NOENT) => {
                match mkdirat(
                    &directory,
                    component.as_os_str(),
                    permission_mode(PRIVATE_DIRECTORY_MODE),
                ) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(rustix_error_to_io(error)),
                }
                openat(
                    &directory,
                    component.as_os_str(),
                    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
                    Mode::empty(),
                )?
            }
            Err(error) => return Err(rustix_error_to_io(error)),
        };
        directory = File::from(descriptor);
    }
    Ok(Some(directory))
}

fn normalized_components(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::CurDir => {}
            Component::Normal(component) => components.push(PathBuf::from(component)),
            Component::ParentDir => {
                components.pop();
            }
            Component::Prefix(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "scoped session directory has an unsupported path prefix",
                ));
            }
        }
    }
    Ok(components)
}

fn rustix_error_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}
