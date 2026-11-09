//! Fallback staging keeps a private name until publication so recovery can reclaim it.

use super::{cstring, directory, from_metadata, link_descriptor, opened, parent, Identity};
use crate::store::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    ffi::{CString, OsStr},
    fs::File,
    io::Write,
    os::fd::{AsRawFd, FromRawFd},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

/// Prefix of fallback staging files; recovery only reclaims names carrying the
/// owning transaction id after it.
pub(super) const STAGING_PREFIX: &str = ".aw-staging-";

/// True when `name` is a fallback staging name owned by `owner`.
pub(super) fn staging_name(name: &OsStr, owner: &str) -> bool {
    name.to_str()
        .is_some_and(|name| name.starts_with(&format!("{STAGING_PREFIX}{owner}-")))
}

#[derive(Debug)]
pub(in crate::store::transaction) struct Staged {
    pub(in crate::store::transaction) identity: Identity,
    file: File,
    fallback: Option<NamedStaging>,
}

/// Anonymous staging file that still has a name: a crash leaves evidence
/// recovery can reclaim, and every error removes the name before it returns.
#[derive(Debug)]
struct NamedStaging {
    directory: File,
    name: Option<CString>,
}

impl NamedStaging {
    fn disarm(&mut self) {
        self.name = None;
    }
}

impl Drop for NamedStaging {
    fn drop(&mut self) {
        if let Some(name) = &self.name {
            // SAFETY: name is one child of the anchored directory; unlinkat never follows symlinks.
            let _ = unsafe { libc::unlinkat(self.directory.as_raw_fd(), name.as_ptr(), 0) };
        }
    }
}

pub(in crate::store::transaction) fn anonymous(
    parent: &Path,
    owner: &str,
    mode: u32,
    write: impl FnOnce(&mut File) -> Result<()>,
) -> Result<Staged> {
    let directory = directory(parent)?;
    let (mut file, fallback) = anonymous_file(&directory, parent, owner, mode)?;
    write(&mut file)?;
    #[cfg(test)]
    check_staging_write(parent)?;
    #[cfg(test)]
    check_staging_sync(parent)?;
    // SAFETY: the file descriptor is live; mode contains only ordinary permission bits.
    if unsafe { libc::fchmod(file.as_raw_fd(), mode) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    file.sync_all()?;
    let metadata = file.metadata()?;
    use std::io::{Seek, SeekFrom};
    file.seek(SeekFrom::Start(0))?;
    let mut hash = Sha256::new();
    std::io::copy(&mut file, &mut hash)?;
    Ok(Staged {
        identity: from_metadata(&metadata, format!("{:x}", hash.finalize())),
        file,
        fallback,
    })
}

// overlayfs and a few other filesystems reject O_TMPFILE; the caller falls
// back to a named staging file that publish moves into place with a rename.
fn unsupported_tmpfile(error: &std::io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EOPNOTSUPP | libc::EINVAL | libc::ENOSYS | libc::EISDIR)
    )
}

fn anonymous_file(
    directory: &File,
    parent: &Path,
    owner: &str,
    mode: u32,
) -> Result<(File, Option<NamedStaging>)> {
    let attempt = opened(
        directory,
        OsStr::new("."),
        libc::O_TMPFILE | libc::O_RDWR,
        mode,
    );
    // When armed, the fault stands in for a filesystem that rejects O_TMPFILE,
    // so the fallback runs even on filesystems that support it.
    #[cfg(test)]
    let attempt = super::faults::check_tmpfile().and(attempt);
    match attempt {
        Ok(file) => Ok((file, None)),
        Err(Error::Io(source)) if unsupported_tmpfile(&source) => {
            let (file, staging) = named_anonymous(directory, parent, owner)?;
            Ok((file, Some(staging)))
        }
        Err(Error::Io(source)) => Err(Error::Filesystem {
            operation: "openat(O_TMPFILE) for anonymous package staging",
            path: parent.to_owned(),
            source,
        }),
        Err(error) => Err(error),
    }
}

static ANONYMOUS_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Claim a fresh private name and install its cleanup guard before anything
/// else can fail on the opened descriptor.
fn named_anonymous(directory: &File, parent: &Path, owner: &str) -> Result<(File, NamedStaging)> {
    // Clone the anchor first: a clone failure must leave no named file behind.
    let held = directory.try_clone()?;
    for _ in 0..64 {
        let sequence = ANONYMOUS_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let name = cstring(OsStr::new(&format!(
            "{STAGING_PREFIX}{owner}-{}-{sequence}",
            std::process::id()
        )))?;
        // SAFETY: held is a live directory and name is one fresh child;
        // O_EXCL claims it without following symlinks.
        let fd = unsafe {
            libc::openat(
                held.as_raw_fd(),
                name.as_ptr(),
                libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_RDWR
                    | libc::O_CLOEXEC
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK,
                0o600,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EEXIST) {
                continue;
            }
            return Err(Error::Filesystem {
                operation: "create named staging file (O_TMPFILE fallback)",
                path: parent.to_owned(),
                source: error,
            });
        }
        // SAFETY: successful openat transferred ownership of a new descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        // The guard exists before the descriptor can be used by fallible code.
        return Ok((
            file,
            NamedStaging {
                directory: held,
                name: Some(name),
            },
        ));
    }
    Err(Error::Filesystem {
        operation: "create named staging file (O_TMPFILE fallback)",
        path: parent.to_owned(),
        source: std::io::Error::from_raw_os_error(libc::EEXIST),
    })
}

pub(in crate::store::transaction) fn publish(mut staged: Staged, destination: &Path) -> Result<()> {
    let (parent, name) = parent(destination)?;
    let name = cstring(name)?;
    match staged.fallback.as_mut() {
        Some(staging) => {
            // The named staging file keeps its private name until this point;
            // renameat2 moves it into place without needing privileges, unlike
            // linking a file that no directory references.
            let Some(source_name) = staging.name.clone() else {
                return Err(Error::Invalid("staging file already published".into()));
            };
            // SAFETY: both names are one-component children of live anchored directory fds.
            if unsafe {
                libc::renameat2(
                    staging.directory.as_raw_fd(),
                    source_name.as_ptr(),
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::RENAME_NOREPLACE,
                )
            } != 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
            staging.disarm();
            staging.directory.sync_all()?;
        }
        None => link_descriptor(&staged.file, &parent, &name)?,
    }
    parent.sync_all()?;
    Ok(())
}

pub(in crate::store::transaction) fn write_bytes(
    parent: &Path,
    owner: &str,
    bytes: &[u8],
    mode: u32,
) -> Result<Staged> {
    anonymous(parent, owner, mode, |file| {
        file.write_all(bytes)?;
        Ok(())
    })
}

/// Remove the staging leftovers a crash between creation and publication can
/// leave in a directory owned by `owner`. Names owned by anything else stay.
pub(in crate::store::transaction) fn remove_staging(
    path: &Path,
    owner: &str,
) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for entry in super::entries(path)? {
        if !entry
            .file_name()
            .is_some_and(|name| staging_name(name, owner))
        {
            continue;
        }
        let (parent, name) = parent(&entry)?;
        let name = cstring(name)?;
        // SAFETY: name is one child of the anchored parent; unlinkat never follows symlinks.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(error.into());
            }
        }
        removed.push(entry);
    }
    Ok(removed)
}

#[cfg(test)]
fn check_staging_write(parent: &Path) -> Result<()> {
    super::faults::check_staging_write(parent)
}

#[cfg(test)]
fn check_staging_sync(parent: &Path) -> Result<()> {
    super::faults::check_staging_sync(parent)
}

#[cfg(test)]
mod tests {
    use super::super::{fail_staging_sync, fail_staging_write, fail_tmpfile};
    use super::*;
    use std::os::fd::AsRawFd;

    fn staging_directory() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir_in("/tmp").expect("tempdir");
        let parent = root.path().join("staging");
        std::fs::create_dir_all(&parent).expect("staging directory");
        (root, parent)
    }

    fn entries(parent: &Path) -> Vec<PathBuf> {
        std::fs::read_dir(parent)
            .expect("list staging directory")
            .map(|entry| entry.expect("entry").path())
            .collect()
    }

    #[test]
    fn anonymous_staging_falls_back_when_tmpfile_is_unsupported() {
        let (_root, parent) = staging_directory();
        let _fault = fail_tmpfile();
        let staged = write_bytes(&parent, "txn", b"fallback payload", 0o644)
            .expect("staging must fall back");
        assert_eq!(staged.identity.mode, 0o644);
        assert_eq!(
            staged.identity.sha256,
            format!("{:x}", Sha256::digest(b"fallback payload"))
        );
        assert_eq!(
            std::fs::read_to_string(format!("/proc/self/fd/{}", staged.file.as_raw_fd()))
                .expect("read staged bytes"),
            "fallback payload"
        );
        publish(staged, &parent.join("published")).expect("publish must move the staged file");
        assert_eq!(
            std::fs::read(parent.join("published")).expect("published bytes"),
            b"fallback payload"
        );
        assert_eq!(
            entries(&parent),
            vec![parent.join("published")],
            "staging file must leave no residue behind"
        );
    }

    #[test]
    fn dropping_an_unpublished_staged_file_removes_the_private_name() {
        let (_root, parent) = staging_directory();
        let _fault = fail_tmpfile();
        let staged = write_bytes(&parent, "txn", b"payload", 0o644).expect("staging");
        assert_eq!(entries(&parent).len(), 1);
        drop(staged);
        assert!(entries(&parent).is_empty());
    }

    #[test]
    fn a_failing_write_removes_the_named_staging_file() {
        let (_root, parent) = staging_directory();
        let _fault = fail_tmpfile();
        let error = anonymous(&parent, "txn", 0o644, |file| {
            file.write_all(b"partial")?;
            Err(Error::Io(std::io::Error::from_raw_os_error(libc::ENOSPC)))
        })
        .expect_err("the staging write must fail");
        assert!(matches!(error, Error::Io(error) if error.raw_os_error() == Some(libc::ENOSPC)));
        assert!(
            entries(&parent).is_empty(),
            "no named staging file may remain"
        );
    }

    #[test]
    fn an_injected_write_failure_removes_the_named_staging_file() {
        let (_root, parent) = staging_directory();
        let _fault = fail_tmpfile();
        let _write = fail_staging_write(&parent);
        assert!(write_bytes(&parent, "txn", b"payload", 0o644).is_err());
        assert!(
            entries(&parent).is_empty(),
            "no named staging file may remain"
        );
    }

    #[test]
    fn an_injected_sync_failure_removes_the_named_staging_file() {
        let (_root, parent) = staging_directory();
        let _fault = fail_tmpfile();
        let _sync = fail_staging_sync(&parent);
        assert!(write_bytes(&parent, "txn", b"payload", 0o644).is_err());
        assert!(
            entries(&parent).is_empty(),
            "no named staging file may remain"
        );
    }

    #[test]
    fn remove_staging_reclaims_only_names_owned_by_the_transaction() {
        let (_root, parent) = staging_directory();
        let owned = parent.join(format!("{STAGING_PREFIX}txn-1-0"));
        let other = parent.join(format!("{STAGING_PREFIX}other-1-0"));
        let payload = parent.join("file-0-new");
        std::fs::write(&owned, b"leftover").expect("owned staging file");
        std::fs::write(&other, b"other").expect("other staging file");
        std::fs::write(&payload, b"payload").expect("payload file");
        let removed = remove_staging(&parent, "txn").expect("reclaim staging");
        assert_eq!(removed, vec![owned]);
        assert!(other.exists(), "unrelated staging names must stay");
        assert!(payload.exists(), "payload files must stay");
    }
}
