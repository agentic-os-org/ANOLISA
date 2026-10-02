//! Durable atomic replacement for general user files written by tools.

use std::ffi::OsString;
use std::fmt;
use std::fs::{File, Permissions};
use std::io::{self, Read, Write};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};

use fs2::FileExt;
use rustix::fs::{openat, renameat, unlinkat, AtFlags, Mode, OFlags};
#[cfg(target_os = "linux")]
use rustix::fs::{openat2, ResolveFlags, CWD};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use super::workspace_fs::WorkspaceWriteTarget;

const TEMP_NAME_ATTEMPTS: usize = 16;

#[derive(Debug)]
pub(super) enum ReplaceError {
    Io {
        operation: &'static str,
        path: PathBuf,
        source: io::Error,
    },
    Conflict {
        path: PathBuf,
    },
    Committed {
        path: PathBuf,
        source: io::Error,
    },
}

impl fmt::Display for ReplaceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io {
                operation,
                path,
                source,
            } => write!(
                formatter,
                "failed to {operation} {}: {source}",
                path.display()
            ),
            Self::Conflict { path } => write!(
                formatter,
                "Edit conflict: {} changed after it was read; no changes were written",
                path.display()
            ),
            Self::Committed { path, source } => write!(
                formatter,
                "replacement of {} was committed, but syncing its parent directory failed: \
                 {source}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for ReplaceError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } | Self::Committed { source, .. } => Some(source),
            Self::Conflict { .. } => None,
        }
    }
}

#[derive(Debug)]
pub(super) struct FileSnapshot {
    bytes: Vec<u8>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileSnapshot {
    fn read(target: &WorkspaceWriteTarget) -> io::Result<Self> {
        let mut file = open_target(target, OFlags::RDONLY | OFlags::CLOEXEC)?;
        let metadata = file.metadata()?;
        let mut bytes = Vec::with_capacity(metadata.len().try_into().unwrap_or(0));
        file.read_to_end(&mut bytes)?;

        Ok(Self {
            bytes,
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
        })
    }

    pub(super) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    fn still_matches(&self, current: &Self) -> bool {
        self.bytes == current.bytes && {
            #[cfg(unix)]
            {
                self.device == current.device && self.inode == current.inode
            }
            #[cfg(not(unix))]
            {
                true
            }
        }
    }
}

pub(super) async fn read_snapshot(
    target: &WorkspaceWriteTarget,
) -> Result<FileSnapshot, ReplaceError> {
    let error_path = target.display_path.clone();
    let target = clone_target(target).map_err(|source| ReplaceError::Io {
        operation: "clone parent directory for snapshot read",
        path: error_path.clone(),
        source,
    })?;
    tokio::task::spawn_blocking(move || FileSnapshot::read(&target))
        .await
        .map_err(|error| ReplaceError::Io {
            operation: "join snapshot read for",
            path: error_path.clone(),
            source: io::Error::other(error),
        })?
        .map_err(|source| ReplaceError::Io {
            operation: "read",
            path: error_path,
            source,
        })
}

pub(super) async fn replace(
    target: &WorkspaceWriteTarget,
    bytes: Vec<u8>,
    expected: Option<FileSnapshot>,
) -> Result<(), ReplaceError> {
    let error_path = target.display_path.clone();
    let target = clone_target(target).map_err(|source| ReplaceError::Io {
        operation: "clone parent directory for atomic replacement",
        path: error_path.clone(),
        source,
    })?;
    tokio::task::spawn_blocking(move || {
        replace_with_hooks(
            &target,
            &bytes,
            expected.as_ref(),
            |_, _| Ok(()),
            |_, _| Ok(()),
            |_| Ok(()),
        )
    })
    .await
    .map_err(|error| ReplaceError::Io {
        operation: "join atomic replacement for",
        path: error_path,
        source: io::Error::other(error),
    })?
}

fn replace_with_hooks<BeforeWrite, BeforeCommit, BeforeDirectorySync>(
    target: &WorkspaceWriteTarget,
    bytes: &[u8],
    expected: Option<&FileSnapshot>,
    before_write: BeforeWrite,
    before_commit: BeforeCommit,
    before_directory_sync: BeforeDirectorySync,
) -> Result<(), ReplaceError>
where
    BeforeWrite: FnOnce(&Path, &Path) -> io::Result<()>,
    BeforeCommit: FnOnce(&Path, &Path) -> io::Result<()>,
    BeforeDirectorySync: FnOnce(&Path) -> io::Result<()>,
{
    let permissions = target_permissions(target)?;
    let (mut temporary, mut guard) = create_temporary(target)?;

    if let Some(permissions) = permissions {
        temporary
            .set_permissions(permissions)
            .map_err(|source| io_error("preserve permissions on", guard.path(), source))?;
    }
    before_write(&target.display_path, guard.path())
        .map_err(|source| io_error("prepare temporary file for", &target.display_path, source))?;
    temporary
        .write_all(bytes)
        .map_err(|source| io_error("write temporary file", guard.path(), source))?;
    temporary
        .flush()
        .map_err(|source| io_error("flush temporary file", guard.path(), source))?;
    temporary
        .sync_all()
        .map_err(|source| io_error("sync temporary file", guard.path(), source))?;
    drop(temporary);

    // The directory inode stays stable across rename, so every helper user
    // serializes the snapshot check and commit on the same advisory lock.
    let directory = open_parent_for_lock(&target.parent)
        .map_err(|source| io_error("open parent directory", &target.display_path, source))?;
    directory
        .lock_exclusive()
        .map_err(|source| io_error("lock parent directory", &target.display_path, source))?;
    before_commit(&target.display_path, guard.path())
        .map_err(|source| io_error("prepare replacement of", &target.display_path, source))?;
    if let Some(expected) = expected {
        let current = match FileSnapshot::read(target) {
            Ok(current) => current,
            Err(source) if source.kind() == io::ErrorKind::NotFound => {
                return Err(ReplaceError::Conflict {
                    path: target.display_path.clone(),
                });
            }
            Err(source) => return Err(io_error("verify", &target.display_path, source)),
        };
        if !expected.still_matches(&current) {
            return Err(ReplaceError::Conflict {
                path: target.display_path.clone(),
            });
        }
    }

    renameat(&target.parent, &guard.name, &target.parent, &target.name)
        .map_err(errno_to_io)
        .map_err(|source| io_error("atomically replace", &target.display_path, source))?;
    guard.disarm();

    let parent_path = target
        .display_path
        .parent()
        .unwrap_or_else(|| Path::new("."));
    before_directory_sync(parent_path).map_err(|source| ReplaceError::Committed {
        path: target.display_path.clone(),
        source,
    })?;
    directory
        .sync_all()
        .map_err(|source| ReplaceError::Committed {
            path: target.display_path.clone(),
            source,
        })?;
    Ok(())
}

fn target_permissions(target: &WorkspaceWriteTarget) -> Result<Option<Permissions>, ReplaceError> {
    #[cfg(target_os = "linux")]
    let flags = OFlags::PATH | OFlags::CLOEXEC;
    #[cfg(target_os = "macos")]
    let flags = OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NONBLOCK;

    match open_target(target, flags).and_then(|file| file.metadata()) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(io_error(
            "inspect permissions for",
            &target.display_path,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "resolved target was replaced by a symbolic link",
            ),
        )),
        Ok(metadata) => Ok(Some(metadata.permissions())),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(io_error(
            "inspect permissions for",
            &target.display_path,
            source,
        )),
    }
}

fn create_temporary(target: &WorkspaceWriteTarget) -> Result<(File, TemporaryGuard), ReplaceError> {
    for _ in 0..TEMP_NAME_ATTEMPTS {
        let name = OsString::from(format!(".cosh-tmp-{}", uuid::Uuid::new_v4()));
        let guard_parent = target.parent.try_clone().map_err(|source| {
            io_error("clone temporary file parent", &target.display_path, source)
        })?;
        match openat(
            &target.parent,
            &name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        ) {
            Ok(file) => {
                let file = File::from(file);
                let guard = TemporaryGuard::new(guard_parent, target, name);
                return Ok((file, guard));
            }
            Err(rustix::io::Errno::EXIST) => continue,
            Err(error) => {
                return Err(io_error(
                    "create temporary file",
                    &target.display_path,
                    errno_to_io(error),
                ))
            }
        }
    }

    Err(io_error(
        "create unique temporary file in",
        &target.display_path,
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "temporary name collision limit reached",
        ),
    ))
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> ReplaceError {
    ReplaceError::Io {
        operation,
        path: path.to_path_buf(),
        source,
    }
}

struct TemporaryGuard {
    parent: File,
    name: OsString,
    display_path: PathBuf,
    armed: bool,
}

impl TemporaryGuard {
    fn new(parent: File, target: &WorkspaceWriteTarget, name: OsString) -> Self {
        Self {
            parent,
            display_path: target.display_path.with_file_name(&name),
            name,
            armed: true,
        }
    }

    fn path(&self) -> &Path {
        &self.display_path
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryGuard {
    fn drop(&mut self) {
        if self.armed {
            let _ = unlinkat(&self.parent, &self.name, AtFlags::empty());
        }
    }
}

fn clone_target(target: &WorkspaceWriteTarget) -> io::Result<WorkspaceWriteTarget> {
    Ok(WorkspaceWriteTarget {
        parent: target.parent.try_clone()?,
        name: target.name.clone(),
        display_path: target.display_path.clone(),
    })
}

fn open_target(target: &WorkspaceWriteTarget, flags: OFlags) -> io::Result<File> {
    openat(
        &target.parent,
        &target.name,
        flags | OFlags::NOFOLLOW,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(errno_to_io)
}

#[cfg(target_os = "linux")]
fn open_parent_for_lock(parent: &File) -> io::Result<File> {
    let descriptor_path = PathBuf::from(format!("/proc/self/fd/{}", parent.as_raw_fd()));
    openat2(
        CWD,
        descriptor_path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
        ResolveFlags::empty(),
    )
    .map(File::from)
    .map_err(errno_to_io)
}

#[cfg(target_os = "macos")]
fn open_parent_for_lock(parent: &File) -> io::Result<File> {
    openat(
        parent,
        ".",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(errno_to_io)
}

fn errno_to_io(error: rustix::io::Errno) -> io::Error {
    io::Error::from_raw_os_error(error.raw_os_error())
}

#[cfg(test)]
pub(super) fn replace_with_before_commit_for_test<BeforeCommit>(
    target: &WorkspaceWriteTarget,
    bytes: &[u8],
    expected: &FileSnapshot,
    before_commit: BeforeCommit,
) -> Result<(), ReplaceError>
where
    BeforeCommit: FnOnce(&Path, &Path) -> io::Result<()>,
{
    replace_with_hooks(
        target,
        bytes,
        Some(expected),
        |_, _| Ok(()),
        before_commit,
        |_| Ok(()),
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::{Seek, SeekFrom};

    #[cfg(target_os = "linux")]
    use std::os::unix::process::ExitStatusExt;
    #[cfg(target_os = "linux")]
    use std::process::{Command, Stdio};
    #[cfg(target_os = "linux")]
    use std::thread;
    #[cfg(target_os = "linux")]
    use std::time::{Duration, Instant};

    #[cfg(unix)]
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use super::*;

    fn temporary_entries(parent: &Path) -> Vec<PathBuf> {
        fs::read_dir(parent)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".cosh-tmp-"))
            })
            .collect()
    }

    fn prepare_test_target(path: &Path) -> WorkspaceWriteTarget {
        let parent = path.parent().expect("test target has a parent");
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("test target name is UTF-8");
        super::super::workspace_fs::WorkspaceFs::new(parent)
            .unwrap()
            .prepare_write(parent, name, false)
            .unwrap()
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn sigkill_before_commit_keeps_existing_target_complete() {
        const CHILD_TARGET_ENV: &str = "COSH_ATOMIC_SIGKILL_CHILD_TARGET";
        const CHILD_READY_ENV: &str = "COSH_ATOMIC_SIGKILL_CHILD_READY";
        const TEST_NAME: &str =
            "tool::atomic_file::tests::sigkill_before_commit_keeps_existing_target_complete";

        if let Some(target) = std::env::var_os(CHILD_TARGET_ENV) {
            let target = PathBuf::from(target);
            let ready = PathBuf::from(
                std::env::var_os(CHILD_READY_ENV).expect("child ready path is configured"),
            );
            let write_target = prepare_test_target(&target);

            let _ = replace_with_hooks(
                &write_target,
                b"complete replacement",
                None,
                |_, _| Ok(()),
                |_, _| {
                    fs::write(&ready, b"ready")?;
                    loop {
                        thread::park();
                    }
                },
                |_| Ok(()),
            );
            unreachable!("the parent must kill the child at the pre-commit barrier");
        }

        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        let ready = directory.path().join("pre-commit-ready");
        fs::write(&target, b"complete original").unwrap();

        let mut child = Command::new(std::env::current_exe().expect("current test executable"))
            .args([TEST_NAME, "--exact", "--nocapture"])
            .env(CHILD_TARGET_ENV, &target)
            .env(CHILD_READY_ENV, &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn atomic replacement child");

        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready.exists() {
            if let Some(status) = child.try_wait().expect("poll atomic replacement child") {
                panic!("atomic replacement child exited before the barrier: {status}");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("atomic replacement child did not reach the pre-commit barrier");
            }
            thread::sleep(Duration::from_millis(10));
        }

        child.kill().expect("send SIGKILL to replacement child");
        let status = child.wait().expect("wait for replacement child");

        assert_eq!(status.signal(), Some(9), "child status: {status}");
        assert_eq!(fs::read(&target).unwrap(), b"complete original");
    }

    #[test]
    #[cfg(unix)]
    fn replacement_uses_rename_and_preserves_permissions() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("source.rs");
        fs::write(&target, b"old content").unwrap();
        let write_target = prepare_test_target(&target);
        fs::set_permissions(&target, Permissions::from_mode(0o640)).unwrap();
        let original_inode = fs::metadata(&target).unwrap().ino();
        let mut original_handle = File::open(&target).unwrap();

        replace_with_hooks(
            &write_target,
            b"new content",
            None,
            |_, _| Ok(()),
            |_, _| Ok(()),
            |_| Ok(()),
        )
        .unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"new content");
        assert_ne!(fs::metadata(&target).unwrap().ino(), original_inode);
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o640
        );
        original_handle.seek(SeekFrom::Start(0)).unwrap();
        let mut original_content = String::new();
        original_handle
            .read_to_string(&mut original_content)
            .unwrap();
        assert_eq!(original_content, "old content");
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn pre_commit_failure_keeps_target_and_cleans_unique_same_directory_temp() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("config.toml");
        fs::write(&target, b"stable").unwrap();
        let write_target = prepare_test_target(&target);
        let mut temporary_path = None;

        let error = replace_with_hooks(
            &write_target,
            b"candidate",
            None,
            |_, _| Ok(()),
            |_, temporary| {
                temporary_path = Some(temporary.to_path_buf());
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "injected interruption",
                ))
            },
            |_| Ok(()),
        )
        .unwrap_err();

        let temporary_path = temporary_path.unwrap();
        assert_eq!(temporary_path.parent(), Some(directory.path()));
        assert!(temporary_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with(".cosh-tmp-"));
        assert!(error.to_string().contains("injected interruption"));
        assert_eq!(fs::read(&target).unwrap(), b"stable");
        assert!(!temporary_path.exists());
    }

    #[test]
    fn rename_failure_keeps_target_and_cleans_temp() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("existing-directory");
        let write_target = prepare_test_target(&target);
        fs::create_dir(&target).unwrap();

        let error = replace_with_hooks(
            &write_target,
            b"candidate",
            None,
            |_, _| Ok(()),
            |_, _| Ok(()),
            |_| Ok(()),
        )
        .unwrap_err();

        assert!(error.to_string().contains("atomically replace"));
        assert!(target.is_dir());
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    fn concurrent_content_change_is_rejected_without_overwrite() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("source.rs");
        fs::write(&target, b"original").unwrap();
        let write_target = prepare_test_target(&target);
        let snapshot = FileSnapshot::read(&write_target).unwrap();

        let error = replace_with_hooks(
            &write_target,
            b"edited",
            Some(&snapshot),
            |_, _| Ok(()),
            |target, _| fs::write(target, b"intervening change"),
            |_| Ok(()),
        )
        .unwrap_err();

        assert!(matches!(error, ReplaceError::Conflict { .. }));
        assert_eq!(fs::read(&target).unwrap(), b"intervening change");
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[test]
    #[cfg(unix)]
    fn same_content_replacement_is_detected_by_file_identity() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("source.rs");
        fs::write(&target, b"original").unwrap();
        let write_target = prepare_test_target(&target);
        let snapshot = FileSnapshot::read(&write_target).unwrap();

        let error = replace_with_hooks(
            &write_target,
            b"edited",
            Some(&snapshot),
            |_, _| Ok(()),
            |target, _| {
                let replacement = target.with_extension("replacement");
                fs::write(&replacement, b"original")?;
                fs::rename(replacement, target)
            },
            |_| Ok(()),
        )
        .unwrap_err();

        assert!(matches!(error, ReplaceError::Conflict { .. }));
        assert_eq!(fs::read(&target).unwrap(), b"original");
    }

    #[test]
    #[cfg(unix)]
    fn existing_permissions_are_applied_before_any_content_is_written() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("credentials.txt");
        fs::write(&target, b"old secret").unwrap();
        let write_target = prepare_test_target(&target);
        fs::set_permissions(&target, Permissions::from_mode(0o600)).unwrap();
        let mut observed_mode = None;
        let mut observed_length = None;

        let error = replace_with_hooks(
            &write_target,
            b"new secret",
            None,
            |_, temporary| {
                let metadata = fs::metadata(temporary)?;
                observed_mode = Some(metadata.permissions().mode() & 0o777);
                observed_length = Some(metadata.len());
                Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "stop before write",
                ))
            },
            |_, _| Ok(()),
            |_| Ok(()),
        )
        .unwrap_err();

        assert_eq!(observed_mode, Some(0o600));
        assert_eq!(observed_length, Some(0));
        assert!(error.to_string().contains("stop before write"));
        assert_eq!(fs::read(&target).unwrap(), b"old secret");
        assert!(temporary_entries(directory.path()).is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_snapshot_commits_allow_only_one_writer() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("source.rs");
        fs::write(&target, b"original").unwrap();
        let write_target = prepare_test_target(&target);
        let first_snapshot = read_snapshot(&write_target).await.unwrap();
        let second_snapshot = read_snapshot(&write_target).await.unwrap();

        let (first, second) = tokio::join!(
            replace(&write_target, b"first".to_vec(), Some(first_snapshot)),
            replace(&write_target, b"second".to_vec(), Some(second_snapshot)),
        );

        assert_eq!(usize::from(first.is_ok()) + usize::from(second.is_ok()), 1);
        assert_eq!(
            usize::from(matches!(first, Err(ReplaceError::Conflict { .. })))
                + usize::from(matches!(second, Err(ReplaceError::Conflict { .. }))),
            1
        );
        assert!(matches!(
            fs::read(&target).unwrap().as_slice(),
            b"first" | b"second"
        ));
    }

    #[test]
    fn parent_sync_failure_reports_that_replacement_was_committed() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("source.rs");
        fs::write(&target, b"old").unwrap();
        let write_target = prepare_test_target(&target);

        let error = replace_with_hooks(
            &write_target,
            b"new",
            None,
            |_, _| Ok(()),
            |_, _| Ok(()),
            |_| Err(io::Error::other("injected sync failure")),
        )
        .unwrap_err();

        assert!(matches!(error, ReplaceError::Committed { .. }));
        assert!(error.to_string().contains("was committed"));
        assert!(error.to_string().contains("injected sync failure"));
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert!(temporary_entries(directory.path()).is_empty());
    }
}
