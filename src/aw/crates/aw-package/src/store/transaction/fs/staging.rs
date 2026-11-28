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

/// True when `name` is exactly the fallback staging name `owner` generates:
/// `.aw-staging-<owner>-<pid>-<sequence>`. A same-prefix name with any other
/// suffix is not ours to reclaim, so recovery never deletes it.
pub(super) fn staging_name(name: &OsStr, owner: &str) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some(rest) = name.strip_prefix(STAGING_PREFIX) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(owner) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('-') else {
        return false;
    };
    let mut parts = rest.split('-');
    let (Some(pid), Some(sequence), None) = (parts.next(), parts.next(), parts.next()) else {
        return false;
    };
    !pid.is_empty()
        && !sequence.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_digit())
        && sequence.bytes().all(|byte| byte.is_ascii_digit())
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
    directory: &File,
    path: &Path,
    owner: &str,
) -> Result<Vec<PathBuf>> {
    let mut removed = Vec::new();
    for entry in super::entries_in(directory, path)? {
        let Some(name) = entry.file_name() else {
            continue;
        };
        if !staging_name(name, owner) {
            continue;
        }
        let name = cstring(name)?;
        // SAFETY: name is one child of the verified directory; unlinkat never follows symlinks.
        if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
            match std::io::Error::last_os_error().raw_os_error() {
                Some(libc::ENOENT) => {}
                // A directory shaped like a staging name is left for the caller's
                // conflict report instead of aborting recovery.
                Some(libc::EISDIR | libc::EPERM | libc::EACCES) => continue,
                _ => return Err(std::io::Error::last_os_error().into()),
            }
        }
        removed.push(entry);
    }
    if !removed.is_empty() {
        directory.sync_all()?;
    }
    Ok(removed)
}

/// Prefix of the workspace marker name; the encoded nonce follows it, so only
/// the transaction that recorded that nonce can create the file.
pub(in crate::store::transaction) const WORKSPACE_MARKER_PREFIX: &str = ".aw-workspace-nonce.";

const WORKSPACE_NONCE_BYTES: usize = 32;

/// Draw the random bytes that become a workspace marker.
pub(in crate::store::transaction) fn new_workspace_nonce() -> Result<Vec<u8>> {
    let mut nonce = vec![0u8; WORKSPACE_NONCE_BYTES];
    // SAFETY: nonce is a live mutable buffer of exactly the requested length.
    let filled = unsafe { libc::getrandom(nonce.as_mut_ptr().cast(), nonce.len(), 0) };
    if filled != nonce.len() as isize {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(nonce)
}

/// The journal records this value instead of the raw marker bytes.
pub(in crate::store::transaction) fn workspace_nonce_hash(nonce: &[u8]) -> String {
    format!("{:x}", Sha256::digest(nonce))
}

/// True when the marker name still carries the nonce the journal recorded.
pub(in crate::store::transaction) fn workspace_nonce_matches(recorded: &str, nonce: &[u8]) -> bool {
    recorded == workspace_nonce_hash(nonce)
}

fn marker_name(nonce: &[u8]) -> String {
    let mut name = String::with_capacity(WORKSPACE_MARKER_PREFIX.len() + 2 * nonce.len());
    name.push_str(WORKSPACE_MARKER_PREFIX);
    for byte in nonce {
        name.push_str(&format!("{byte:02x}"));
    }
    name
}

fn decode_hex(encoded: &str) -> Option<Vec<u8>> {
    if !encoded.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(encoded.len() / 2);
    let mut digits = encoded.bytes();
    while let (Some(high), Some(low)) = (digits.next(), digits.next()) {
        let high = (high as char).to_digit(16)?;
        let low = (low as char).to_digit(16)?;
        bytes.push((high * 16 + low) as u8);
    }
    Some(bytes)
}

/// The nonce encoded in a marker name, when `name` has the marker shape.
pub(in crate::store::transaction) fn workspace_marker_nonce(name: &OsStr) -> Option<Vec<u8>> {
    let encoded = name.to_str()?.strip_prefix(WORKSPACE_MARKER_PREFIX)?;
    let nonce = decode_hex(encoded)?;
    (nonce.len() == WORKSPACE_NONCE_BYTES).then_some(nonce)
}

/// Publish the marker without an intermediate name: exclusive creation keeps a
/// second writer out, and the encoded nonce in the name is the ownership proof
/// recovery compares against the journal.
pub(in crate::store::transaction) fn write_workspace_marker(
    workspace: &Path,
    nonce: &[u8],
) -> Result<()> {
    let directory = super::directory(workspace)?;
    let name = marker_name(nonce);
    let file = opened(
        &directory,
        OsStr::new(&name),
        libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
        0o600,
    )?;
    file.sync_all()?;
    drop(file);
    directory.sync_all()?;
    Ok(())
}

/// Delete one verified child. A name that stopped being a regular file between
/// validation and removal is reported as `false` so the caller raises a conflict
/// instead of failing recovery with an opaque `EISDIR`.
fn unlink(directory: &File, name: &CString) -> Result<bool> {
    // SAFETY: `name` is a single child of the verified workspace descriptor.
    if unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) } != 0 {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(libc::ENOENT) => Ok(true),
            Some(libc::EISDIR | libc::EPERM | libc::EACCES) => Ok(false),
            _ => Err(error.into()),
        };
    }
    Ok(true)
}

/// True when `name` is still a regular file reachable through the descriptor.
fn regular(directory: &File, name: &CString) -> Result<bool> {
    let mut status: libc::stat = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is a single child of the verified workspace descriptor.
    if unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            &mut status,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        let error = std::io::Error::last_os_error();
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(false);
        }
        return Err(error.into());
    }
    Ok(status.st_mode & libc::S_IFMT == libc::S_IFREG)
}

/// Remove the workspace only when its marker proves ownership by `recorded` and
/// every other entry is a staging name `owner` generates. Staging files go first
/// and the directory is synced before the marker is retired, so an interrupted
/// cleanup still leaves the marker the next pass needs. Unknown content is never
/// touched: the function reports `false` so the caller raises a conflict.
pub(in crate::store::transaction) fn clear_workspace(
    directory: &File,
    workspace: &Path,
    owner: &str,
    recorded: &str,
) -> Result<bool> {
    let mut staging = Vec::new();
    let mut marker = None;
    for entry in super::entries_in(directory, workspace)? {
        let Some(name) = entry.file_name() else {
            return Ok(false);
        };
        let staged = staging_name(name, owner);
        let claims_ownership = !staged
            && marker.is_none()
            && workspace_marker_nonce(name)
                .is_some_and(|nonce| workspace_nonce_matches(recorded, &nonce));
        if !staged && !claims_ownership {
            return Ok(false);
        }
        let name = cstring(name)?;
        if !regular(directory, &name)? {
            return Ok(false);
        }
        if staged {
            staging.push(name);
        } else {
            marker = Some(name);
        }
    }
    let Some(marker) = marker else {
        return Ok(false);
    };
    for name in &staging {
        if !unlink(directory, name)? {
            return Ok(false);
        }
    }
    if !staging.is_empty() {
        directory.sync_all()?;
    }
    if !unlink(directory, &marker)? {
        return Ok(false);
    }
    directory.sync_all()?;
    let identity = super::identity_of(directory)?;
    super::rmdir(workspace, Some(&identity))
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
    fn staging_name_requires_the_complete_generated_format() {
        assert!(staging_name(OsStr::new(".aw-staging-txn-1-0"), "txn"));
        assert!(staging_name(OsStr::new(".aw-staging-txn-42-17"), "txn"));
        assert!(!staging_name(OsStr::new(".aw-staging-txn-notes"), "txn"));
        assert!(!staging_name(OsStr::new(".aw-staging-txn-1"), "txn"));
        assert!(!staging_name(OsStr::new(".aw-staging-txn-1-2-3"), "txn"));
        assert!(!staging_name(OsStr::new(".aw-staging-txn--1"), "txn"));
        assert!(!staging_name(OsStr::new(".aw-staging-other-1-0"), "txn"));
    }

    #[test]
    fn marker_names_round_trip_the_nonce_and_reject_foreign_shapes() {
        let nonce = vec![0xab; WORKSPACE_NONCE_BYTES];
        let name = marker_name(&nonce);
        assert!(name.starts_with(WORKSPACE_MARKER_PREFIX));
        assert_eq!(
            workspace_marker_nonce(OsStr::new(&name)),
            Some(nonce.clone())
        );
        assert!(workspace_nonce_matches(
            &workspace_nonce_hash(&nonce),
            &workspace_marker_nonce(OsStr::new(&name)).unwrap()
        ));
        assert_eq!(
            workspace_marker_nonce(OsStr::new(".aw-workspace-nonce.tmp")),
            None
        );
        assert_eq!(
            workspace_marker_nonce(OsStr::new(".aw-workspace-nonce.zz")),
            None
        );
        assert_eq!(
            workspace_marker_nonce(OsStr::new(".aw-workspace-nonce.abcd")),
            None
        );
    }

    #[test]
    fn clear_workspace_requires_the_recorded_marker() {
        let (_root, parent) = staging_directory();
        let nonce = vec![7u8; WORKSPACE_NONCE_BYTES];
        let workspace = parent.join("workspace");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::write(workspace.join(marker_name(&nonce)), b"").expect("marker");
        std::fs::write(workspace.join(format!("{STAGING_PREFIX}txn-1-0")), b"stage")
            .expect("stage");
        let directory = std::fs::File::open(&workspace).expect("workspace");
        assert!(!clear_workspace(&directory, &workspace, "txn", &"0".repeat(64)).unwrap());
        assert!(workspace.join(marker_name(&nonce)).exists());
        assert!(
            clear_workspace(&directory, &workspace, "txn", &workspace_nonce_hash(&nonce)).unwrap()
        );
        assert!(!workspace.exists());
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
        let directory = std::fs::File::open(&parent).expect("staging directory");
        let removed = remove_staging(&directory, &parent, "txn").expect("reclaim staging");
        assert_eq!(removed, vec![owned]);
        assert!(other.exists(), "unrelated staging names must stay");
        assert!(payload.exists(), "payload files must stay");
    }
}
