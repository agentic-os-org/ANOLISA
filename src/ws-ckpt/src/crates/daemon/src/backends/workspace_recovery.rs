use std::fmt::Write as _;
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use nix::errno::Errno;
use nix::fcntl::{renameat2, OFlag, RenameFlags};
use nix::unistd::{chown, Gid, Uid};
use tempfile::TempDir;
use tokio::process::Command;
use tracing::{info, warn};

const STAGING_PREFIX: &str = ".ws-ckpt-recover.";
const DISPLACED_PREFIX: &str = ".ws-ckpt-displaced.";
const MARKER_SUFFIX: &str = ".staging";
const MAX_MARKER_BYTES: usize = 512;
const MAX_HANDLE_SZ: usize = 128;
const FILEID_ROOT: libc::c_int = 0;
const FILEID_INVALID: libc::c_int = 0xff;

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileHandle {
    handle_type: i32,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct DirIdentity {
    dev: u64,
    ino: u64,
    handle: Option<FileHandle>,
}

#[repr(C)]
struct RawFileHandle {
    handle_bytes: libc::c_uint,
    handle_type: libc::c_int,
    f_handle: [u8; MAX_HANDLE_SZ],
}

/// Query the kernel exportfs file handle for an open directory descriptor via
/// `name_to_handle_at(..., AT_EMPTY_PATH)`. Unlike a bare `(dev, ino)` pair,
/// exportfs file handles carry the kernel inode generation (or monotonic btrfs
/// objectid) so a recycled inode after delete+recreate produces a distinct
/// handle. Returns `None` on filesystems that do not support export handles
/// (for example `overlayfs` without `index=on`), allowing callers to fail
/// closed instead of guessing ownership.
fn dir_file_handle(dir_fd: &fs::File) -> Option<FileHandle> {
    let mut raw = RawFileHandle {
        handle_bytes: MAX_HANDLE_SZ as libc::c_uint,
        handle_type: 0,
        f_handle: [0u8; MAX_HANDLE_SZ],
    };
    let mut mount_id: libc::c_int = 0;
    // SAFETY: `dir_fd` is a valid open file descriptor, `c""` is NUL-terminated,
    // `raw.handle_bytes` matches the capacity of `raw.f_handle`, and `mount_id`
    // is a valid out-pointer.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            dir_fd.as_raw_fd(),
            c"".as_ptr(),
            &mut raw as *mut RawFileHandle,
            &mut mount_id as *mut libc::c_int,
            libc::AT_EMPTY_PATH,
        )
    };
    if rc != 0 {
        return None;
    }
    let len = raw.handle_bytes as usize;
    if len == 0
        || len > MAX_HANDLE_SZ
        || raw.handle_type == FILEID_ROOT
        || raw.handle_type == FILEID_INVALID
    {
        return None;
    }
    Some(FileHandle {
        handle_type: raw.handle_type,
        bytes: raw.f_handle[..len].to_vec(),
    })
}

/// Inspect a directory without following symlinks and without requiring read
/// permission (`O_PATH | O_DIRECTORY | O_NOFOLLOW`), returning its `(dev, ino)`
/// and optional kernel file handle from the same open descriptor.
fn inspect_dir(path: &Path) -> std::io::Result<DirIdentity> {
    let dir_fd = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_PATH | libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(path)?;
    let meta = dir_fd.metadata()?;
    if !meta.is_dir() {
        return Err(std::io::Error::from_raw_os_error(libc::ENOTDIR));
    }
    let handle = dir_file_handle(&dir_fd);
    Ok(DirIdentity {
        dev: meta.dev(),
        ino: meta.ino(),
        handle,
    })
}

fn matches_dir_identity(path: &Path, expected: &DirIdentity) -> bool {
    let Ok(current) = inspect_dir(path) else {
        return false;
    };
    if current.dev != expected.dev || current.ino != expected.ino {
        return false;
    }
    match (&expected.handle, &current.handle) {
        (Some(exp), Some(cur)) => exp == cur,
        (None, None) => true,
        _ => false,
    }
}

/// Restore owner read/write/traverse bits on directories inside a staging tree
/// without following symlinks, so restrictive workspace modes (for example
/// `0555`) cannot prevent cleanup.
fn make_dirs_writable(root: &Path) {
    let Ok(meta) = fs::symlink_metadata(root) else {
        return;
    };
    if !meta.is_dir() {
        return;
    }
    let mode = meta.permissions().mode();
    if mode & 0o700 != 0o700 {
        let _ = fs::set_permissions(root, fs::Permissions::from_mode(mode | 0o700));
    }
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
            make_dirs_writable(&entry.path());
        }
    }
}

/// Injectively encode `ws_id` using only ASCII alphanumeric, `-`, and `_xx`
/// escapes so `.` is reserved as an unambiguous delimiter between the complete
/// workspace ID and the random suffix (preventing `ws-abcdef` from matching
/// `ws-abcdef-2`).
fn encode_ws_id(ws_id: &str) -> String {
    if ws_id.is_empty() {
        return "_".to_string();
    }
    let mut encoded = String::with_capacity(ws_id.len());
    for &b in ws_id.as_bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' {
            encoded.push(b as char);
        } else {
            let _ = write!(encoded, "_{b:02x}");
        }
    }
    encoded
}

fn staging_dir_prefix(ws_id: &str) -> String {
    format!("{STAGING_PREFIX}{}.", encode_ws_id(ws_id))
}

fn marker_path_for(staging_path: &Path) -> Result<PathBuf> {
    let mut name = staging_path
        .file_name()
        .context("staging path has no file name")?
        .to_os_string();
    name.push(MARKER_SUFFIX);
    Ok(staging_path.with_file_name(name))
}

/// Cleanup guard for our own staging directory and its companion `.staging`
/// ownership marker. If the staged workspace tree had restrictive directory
/// permissions (for example `0555`), restore `0700` before `TempDir::drop`
/// removes its contents. Drop also verifies directory identity (including the
/// kernel file handle when available) before touching the path, and calling
/// `keep()` disarms cleanup so a displaced external entry is never modified or
/// deleted.
struct StagedDir {
    dir: Option<TempDir>,
    identity: DirIdentity,
    marker: Option<PathBuf>,
}

impl StagedDir {
    fn create(parent: &Path, ws_id: &str) -> Result<Self> {
        let dir_prefix = staging_dir_prefix(ws_id);
        let dir = tempfile::Builder::new()
            .prefix(&dir_prefix)
            .rand_bytes(8)
            .tempdir_in(parent)
            .with_context(|| format!("failed to create recovery staging dir in {parent:?}"))?;
        let identity = inspect_dir(dir.path())?;
        // Only emit a `.staging` sweep marker when the filesystem provides a
        // generation-bearing export handle; otherwise fail closed on crash
        // leftovers rather than relying on a bare inode number that can alias
        // a recycled inode.
        let marker = if let Some(handle) = &identity.handle {
            let marker = marker_path_for(dir.path())?;
            let encoded_id = encode_ws_id(ws_id);
            let write_result = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(OFlag::O_NOFOLLOW.bits())
                .open(&marker)
                .and_then(|mut f| {
                    write!(
                        f,
                        "{encoded_id}\n{}\n{}:{}\n",
                        identity.ino,
                        handle.handle_type,
                        hex::encode(&handle.bytes)
                    )
                });
            if let Err(e) = write_result {
                let _ = fs::remove_file(&marker);
                return Err(e).with_context(|| {
                    format!(
                        "failed to write recovery staging marker {}",
                        marker.display()
                    )
                });
            }
            Some(marker)
        } else {
            None
        };
        Ok(Self {
            dir: Some(dir),
            identity,
            marker,
        })
    }

    fn path(&self) -> &Path {
        match self.dir.as_ref() {
            Some(dir) => dir.path(),
            None => Path::new(""),
        }
    }

    fn remove_marker(&self) -> Result<()> {
        let Some(marker) = &self.marker else {
            return Ok(());
        };
        match fs::remove_file(marker) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e).with_context(|| {
                format!(
                    "failed to remove recovery staging marker {}",
                    marker.display()
                )
            }),
        }
    }

    fn keep(mut self) -> PathBuf {
        if let Some(marker) = self.marker.take() {
            let _ = fs::remove_file(marker);
        }
        match self.dir.take() {
            Some(dir) => dir.keep(),
            None => PathBuf::new(),
        }
    }
}

impl Drop for StagedDir {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            if let Some(marker) = self.marker.take() {
                let _ = fs::remove_file(marker);
            }
            if matches_dir_identity(dir.path(), &self.identity) {
                make_dirs_writable(dir.path());
                drop(dir);
            } else {
                // Never delete or chmod an entry that is no longer our staged directory.
                let _ = dir.keep();
            }
        }
    }
}

/// Move a displaced external entry out of the staging namespace (best-effort,
/// never overwriting an existing target) and return its retained path. Even if
/// the rename fails, the staging marker was already removed before EXCHANGE so
/// future stale-staging sweeps will never reclaim it.
fn retain_displaced(staging: StagedDir) -> PathBuf {
    let displaced = staging.keep();
    if let Some(name) = displaced.file_name().and_then(|n| n.to_str()) {
        if let Some(rest) = name.strip_prefix(STAGING_PREFIX) {
            let retained = displaced.with_file_name(format!("{DISPLACED_PREFIX}{rest}"));
            if renameat2(
                None,
                &displaced,
                None,
                &retained,
                RenameFlags::RENAME_NOREPLACE,
            )
            .is_ok()
            {
                return retained;
            }
        }
    }
    displaced
}

/// Match `.ws-ckpt-recover.<encoded_ws_id>.<rand>.staging` strictly on the
/// complete encoded workspace ID and return the paired staging directory name.
fn staging_dir_name_for_marker(file_name: &str, expected_encoded_id: &str) -> Option<String> {
    let stem = file_name.strip_suffix(MARKER_SUFFIX)?;
    let rest = stem.strip_prefix(STAGING_PREFIX)?;
    let (id_part, rand_part) = rest.split_once('.')?;
    if id_part != expected_encoded_id || rand_part.is_empty() || rand_part.contains('.') {
        return None;
    }
    Some(stem.to_string())
}

/// Read a companion `.staging` ownership marker without following symlinks or
/// blocking on non-regular files, returning the recorded staging `(ino, handle)`
/// only if its encoded workspace ID matches `expected_encoded_id` and it carries
/// a non-empty valid export file handle.
fn read_staging_marker(marker_path: &Path, expected_encoded_id: &str) -> Option<(u64, FileHandle)> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(OFlag::O_NOFOLLOW.bits() | OFlag::O_NONBLOCK.bits())
        .open(marker_path)
        .ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > MAX_MARKER_BYTES as u64 {
        return None;
    }
    let mut raw = Vec::new();
    file.take((MAX_MARKER_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() > MAX_MARKER_BYTES {
        return None;
    }
    let text = std::str::from_utf8(&raw).ok()?;
    let mut lines = text.lines();
    let recorded_id = lines.next()?;
    let ino = lines.next()?;
    let handle_line = lines.next()?;
    if lines.next().is_some() || recorded_id != expected_encoded_id {
        return None;
    }
    let ino: u64 = ino.parse().ok()?;
    let (handle_type, handle_hex) = handle_line.split_once(':')?;
    let handle_type: i32 = handle_type.parse().ok()?;
    if handle_type == FILEID_ROOT || handle_type == FILEID_INVALID {
        return None;
    }
    let bytes = hex::decode(handle_hex).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_HANDLE_SZ {
        return None;
    }
    Some((ino, FileHandle { handle_type, bytes }))
}

/// Best-effort sweep of orphaned staging directories belonging to `ws_id` left
/// behind in `parent` if the daemon died before publication. Only entries
/// proven by a matching `.staging` ownership marker with the exact complete
/// workspace ID, same-filesystem inode, AND matching kernel export file handle
/// (preventing recycled-inode aliasing) are removed; unmarked directories,
/// displaced foreign entries, symlinks, mountpoints, recycled-inode
/// replacements, and sibling workspaces (such as `ws-abcdef-2` when recovering
/// `ws-abcdef`) are never touched.
fn cleanup_stale_staging(parent: &Path, ws_id: &str) {
    let encoded_id = encode_ws_id(ws_id);
    let Ok(parent_meta) = fs::metadata(parent) else {
        return;
    };
    let parent_dev = parent_meta.dev();
    let Ok(entries) = fs::read_dir(parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(dir_name) = name
            .to_str()
            .and_then(|s| staging_dir_name_for_marker(s, &encoded_id))
        else {
            continue;
        };
        if !entry.file_type().is_ok_and(|ft| ft.is_file()) {
            continue;
        }
        let marker_path = entry.path();
        let Some((expected_ino, expected_handle)) = read_staging_marker(&marker_path, &encoded_id)
        else {
            continue;
        };
        let stale = parent.join(&dir_name);
        match inspect_dir(&stale) {
            Ok(cur)
                if cur.dev == parent_dev
                    && cur.ino == expected_ino
                    && cur.handle.as_ref() == Some(&expected_handle) =>
            {
                make_dirs_writable(&stale);
                if let Err(e) = fs::remove_dir_all(&stale) {
                    warn!(
                        "failed to clean up stale recovery staging dir {}: {e}",
                        stale.display()
                    );
                } else {
                    let _ = fs::remove_file(&marker_path);
                    info!("cleaned up stale recovery staging dir {}", stale.display());
                }
            }
            Ok(_) => {
                warn!(
                    "skipping recovery path {} whose identity no longer matches marker {}",
                    stale.display(),
                    marker_path.display()
                );
                let _ = fs::remove_file(&marker_path);
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {
                let _ = fs::remove_file(&marker_path);
            }
            Err(e) if matches!(e.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                warn!(
                    "skipping non-directory recovery path {} for marker {}",
                    stale.display(),
                    marker_path.display()
                );
                let _ = fs::remove_file(&marker_path);
            }
            Err(e) => {
                warn!(
                    "failed to inspect stale recovery staging dir {}: {e}",
                    stale.display()
                );
            }
        }
    }
}

/// Return the managed link's identity, or None for a vacant destination.
fn managed_link(path: &Path, source: &Path) -> Result<Option<(u64, u64)>> {
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_symlink() => match path.canonicalize() {
            Ok(target) if target == source => Ok(Some((meta.dev(), meta.ino()))),
            Ok(_) | Err(_) => {
                bail!("cannot recover: {path:?} points to a different workspace")
            }
        },
        Ok(meta) => {
            let kind = if meta.is_dir() {
                "a regular directory"
            } else {
                "occupied"
            };
            bail!("cannot recover: {} is {kind}, not a workspace symlink; move or rename it first to avoid data loss", path.display());
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to inspect {path:?}")),
    }
}

fn publish(staging: StagedDir, path: &Path, source: &Path) -> Result<()> {
    publish_with(staging, path, source, |from, to, flags| {
        renameat2(None, from, None, to, flags)
    })
}

fn publish_with<F>(staging: StagedDir, path: &Path, source: &Path, mut exchange: F) -> Result<()>
where
    F: FnMut(&Path, &Path, RenameFlags) -> std::result::Result<(), Errno>,
{
    let staged_identity = staging.identity.clone();
    let link = managed_link(path, source)?;
    let flags = if link.is_some() {
        RenameFlags::RENAME_EXCHANGE
    } else {
        RenameFlags::RENAME_NOREPLACE
    };
    // Disarm stale-staging cleanup BEFORE any exchange can place a foreign
    // entry at `staging.path()`.
    staging.remove_marker()?;
    match exchange(staging.path(), path, flags) {
        Ok(()) => (),
        // Unlink + rename would expose a gap and can strand the workspace.
        Err(Errno::ENOSYS | Errno::EINVAL) => {
            bail!("filesystem does not support atomic recovery publication at {path:?}")
        }
        Err(e) => return Err(e).with_context(|| format!("failed to publish recovery at {path:?}")),
    }
    if let Some(identity) = link {
        // EXCHANGE has no inode predicate. If a different entry raced the
        // validation, put it back instead of deleting somebody else's data.
        let unlink = fs::symlink_metadata(staging.path()).and_then(|m| {
            if (m.dev(), m.ino()) != identity || !m.is_symlink() {
                return Err(std::io::Error::other(
                    "destination changed during publication",
                ));
            }
            fs::remove_file(staging.path())
        });
        if let Err(e) = unlink {
            if let Err(undo) = exchange(staging.path(), path, RenameFlags::RENAME_EXCHANGE) {
                let displaced = retain_displaced(staging);
                return Err(undo).with_context(|| format!("failed to undo recovery publication at {path:?} after {e}; displaced entry retained at {displaced:?}"));
            }
            // Only clean up the directory we staged, even if the destination
            // was replaced a second time while the exchange was being undone.
            if !matches_dir_identity(staging.path(), &staged_identity) {
                let displaced = retain_displaced(staging);
                bail!("destination changed again during recovery; displaced entry retained at {displaced:?}");
            }
            return Err(e).context("recovery publication was rolled back");
        }
    }
    // The staging name is now vacant and no longer ours to clean up.
    let _ = staging.keep();
    Ok(())
}

pub(super) async fn restore(ws_id: &str, subvol: &Path, path: &Path) -> Result<()> {
    let ws_id_owned = ws_id.to_string();
    let subvol_owned = subvol.to_path_buf();
    let path_owned = path.to_path_buf();
    let (source, meta, staging) = tokio::task::spawn_blocking(move || -> Result<_> {
        let source = subvol_owned.canonicalize().with_context(|| format!(
            "cannot recover: subvolume {subvol_owned:?} is missing or unreadable; no data was restored. If it was deleted, use `ws-ckpt unregister -w {path_owned:?} --force` to remove only the registration while retaining backups and snapshots"
        ))?;
        managed_link(&path_owned, &source)?;
        let meta = fs::metadata(&source)?;
        let parent = path_owned
            .parent()
            .context("workspace has no parent directory")?;
        cleanup_stale_staging(parent, &ws_id_owned);
        let staging = StagedDir::create(parent, &ws_id_owned)
            .with_context(|| format!("failed to stage recovery beside {path_owned:?}"))?;
        Ok((source, meta, staging))
    })
    .await
    .context("recovery staging task failed")??;

    let staged_path = staging.path().to_path_buf();
    let status = match Command::new("rsync")
        .args(["-aHAX", "--delete", "--"])
        .arg(source.join(""))
        .arg(&staged_path)
        .kill_on_drop(true)
        .status()
        .await
    {
        Ok(status) => status,
        Err(e) => {
            let _ = tokio::task::spawn_blocking(move || drop(staging)).await;
            return Err(e).context("failed to run rsync");
        }
    };
    if !status.success() {
        let _ = tokio::task::spawn_blocking(move || drop(staging)).await;
        bail!(
            "rsync failed restoring {source:?} -> {path:?}, exit: {:?}; workspace and snapshots preserved for retry",
            status.code()
        );
    }

    let path_owned = path.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        if let Err(e) = chown(
            staging.path(),
            Some(Uid::from_raw(meta.uid())),
            Some(Gid::from_raw(meta.gid())),
        ) {
            warn!(
                "failed to restore ownership on {}: {}",
                path_owned.display(),
                e
            );
        }
        if let Err(e) = fs::set_permissions(staging.path(), meta.permissions()) {
            warn!(
                "failed to restore permissions on {}: {}",
                path_owned.display(),
                e
            );
        }
        publish(staging, &path_owned, &source)
    })
    .await
    .context("recovery publication task failed")??;

    info!("restored workspace contents to {}", path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    fn make_staged(parent: &Path, ws_id: &str) -> StagedDir {
        let staged = StagedDir::create(parent, ws_id).unwrap();
        fs::write(staged.path().join("staged.txt"), "recovered").unwrap();
        staged
    }

    /// Simulate SIGKILL mid-rsync by disarming `Drop` while leaving both the
    /// staging directory and its optional `.staging` marker file on disk.
    fn abandon_as_crash(mut staged: StagedDir) -> (PathBuf, Option<PathBuf>) {
        let dir = staged.dir.take().unwrap().keep();
        let marker = staged.marker.take();
        (dir, marker)
    }

    fn displaced_path_for(staged_path: &Path) -> PathBuf {
        let name = staged_path.file_name().unwrap().to_str().unwrap();
        let rest = name.strip_prefix(STAGING_PREFIX).unwrap();
        staged_path.with_file_name(format!("{DISPLACED_PREFIX}{rest}"))
    }

    #[test]
    fn publish_rolls_back_and_cleans_staging_on_identity_mismatch() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("subvol");
        fs::create_dir(&source).unwrap();
        let ws = tmp.path().join("workspace");
        symlink(&source, &ws).unwrap();

        let staging = make_staged(tmp.path(), "ws-abc123");
        let staged_path = staging.path().to_path_buf();
        let marker_path = staging.marker.clone();
        let foreign = tmp.path().join("foreign-entry");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("precious.txt"), "keep-me").unwrap();

        let mut calls = 0usize;
        let err = publish_with(staging, &ws, &source, |from, to, flags| {
            calls += 1;
            if calls == 1 {
                assert_eq!(flags, RenameFlags::RENAME_EXCHANGE);
                // Simulate a third party replacing the managed symlink right
                // after managed_link() validated it and before RENAME_EXCHANGE.
                fs::remove_file(to).unwrap();
                fs::rename(&foreign, to).unwrap();
            }
            renameat2(None, from, None, to, flags)
        })
        .unwrap_err();

        assert_eq!(calls, 2);
        assert!(err.to_string().contains("rolled back"), "{err:#}");
        assert!(ws.is_dir() && !fs::symlink_metadata(&ws).unwrap().is_symlink());
        assert_eq!(
            fs::read_to_string(ws.join("precious.txt")).unwrap(),
            "keep-me"
        );
        assert!(!staged_path.exists(), "staging dir should be cleaned up");
        if let Some(marker) = marker_path {
            assert!(!marker.exists(), "staging marker should be cleaned up");
        }
    }

    #[test]
    fn publish_keeps_displaced_entry_when_undo_exchange_fails() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("subvol");
        fs::create_dir(&source).unwrap();
        let ws = tmp.path().join("workspace");
        symlink(&source, &ws).unwrap();

        let staging = make_staged(tmp.path(), "ws-abc123");
        let staged_path = staging.path().to_path_buf();
        let marker_path = staging.marker.clone();
        let retained_path = displaced_path_for(&staged_path);
        let foreign = tmp.path().join("foreign-entry");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("precious.txt"), "do-not-delete").unwrap();

        let mut calls = 0usize;
        let err = publish_with(staging, &ws, &source, |from, to, flags| {
            calls += 1;
            if calls == 1 {
                fs::remove_file(to).unwrap();
                fs::rename(&foreign, to).unwrap();
                renameat2(None, from, None, to, flags)
            } else {
                Err(Errno::EIO)
            }
        })
        .unwrap_err();

        assert_eq!(calls, 2);
        assert!(
            err.to_string().contains("displaced entry retained"),
            "{err:#}"
        );
        assert!(
            err.to_string()
                .contains(&retained_path.display().to_string()),
            "{err:#}"
        );
        assert!(
            !staged_path.exists(),
            "displaced entry moved out of staging prefix"
        );
        if let Some(marker) = marker_path {
            assert!(!marker.exists(), "staging marker removed before exchange");
        }
        assert_eq!(
            fs::read_to_string(retained_path.join("precious.txt")).unwrap(),
            "do-not-delete"
        );
    }

    #[test]
    fn publish_keeps_displaced_entry_when_destination_changes_again_during_undo() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("subvol");
        fs::create_dir(&source).unwrap();
        let ws = tmp.path().join("workspace");
        symlink(&source, &ws).unwrap();

        let staging = make_staged(tmp.path(), "ws-abc123");
        let staged_path = staging.path().to_path_buf();
        let marker_path = staging.marker.clone();
        let retained_path = displaced_path_for(&staged_path);
        let foreign_a = tmp.path().join("foreign-a");
        fs::create_dir(&foreign_a).unwrap();
        fs::write(foreign_a.join("a.txt"), "first").unwrap();
        let foreign_b = tmp.path().join("foreign-b");
        fs::create_dir(&foreign_b).unwrap();
        fs::write(foreign_b.join("b.txt"), "second").unwrap();

        let mut calls = 0usize;
        let err = publish_with(staging, &ws, &source, |from, to, flags| {
            calls += 1;
            if calls == 1 {
                fs::remove_file(to).unwrap();
                fs::rename(&foreign_a, to).unwrap();
            } else if calls == 2 {
                // Before undo exchange, the destination changes a second time,
                // so undo brings foreign_b (not our staged dir) to staged_path.
                fs::remove_dir_all(to).unwrap();
                fs::rename(&foreign_b, to).unwrap();
            }
            renameat2(None, from, None, to, flags)
        })
        .unwrap_err();

        assert_eq!(calls, 2);
        assert!(
            err.to_string()
                .contains("destination changed again during recovery"),
            "{err:#}"
        );
        assert_eq!(fs::read_to_string(ws.join("a.txt")).unwrap(), "first");
        assert!(!staged_path.exists());
        if let Some(marker) = marker_path {
            assert!(!marker.exists());
        }
        assert_eq!(
            fs::read_to_string(retained_path.join("b.txt")).unwrap(),
            "second"
        );
    }

    #[tokio::test]
    async fn retry_after_undo_enoent_preserves_retained_foreign_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("subvol");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("workspace.txt"), "subvol-data").unwrap();
        let ws = tmp.path().join("workspace");
        symlink(&source, &ws).unwrap();

        let staging = make_staged(tmp.path(), "ws-abcdef");
        let staged_path = staging.path().to_path_buf();
        let retained_path = displaced_path_for(&staged_path);
        let foreign = tmp.path().join("foreign-entry");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("external.txt"), "must-survive-retry").unwrap();
        let moved_away = tmp.path().join("moved-away");

        // Reproduce an undo EXCHANGE ENOENT using real renameat2 calls:
        // 1. Concurrent replacement takes `ws` before the first EXCHANGE.
        // 2. First EXCHANGE puts our staged dir at `ws` and `foreign` at `staged_path`.
        // 3. Another actor moves `ws` away before the undo EXCHANGE, so undo fails ENOENT.
        let mut calls = 0usize;
        let err = publish_with(staging, &ws, &source, |from, to, flags| {
            calls += 1;
            if calls == 1 {
                fs::remove_file(to).unwrap();
                fs::rename(&foreign, to).unwrap();
            } else if calls == 2 {
                fs::rename(to, &moved_away).unwrap();
            }
            renameat2(None, from, None, to, flags)
        })
        .unwrap_err();
        assert_eq!(calls, 2);
        assert!(
            err.to_string().contains("displaced entry retained"),
            "{err:#}"
        );
        assert!(!ws.exists(), "destination should now be vacant");
        assert_eq!(
            fs::read_to_string(retained_path.join("external.txt")).unwrap(),
            "must-survive-retry"
        );

        // Retrying restore() accepts the now-vacant destination and runs stale
        // cleanup; the retained external directory must remain untouched.
        restore("ws-abcdef", &source, &ws).await.unwrap();
        assert_eq!(
            fs::read_to_string(ws.join("workspace.txt")).unwrap(),
            "subvol-data"
        );
        assert_eq!(
            fs::read_to_string(retained_path.join("external.txt")).unwrap(),
            "must-survive-retry"
        );
    }

    #[tokio::test]
    async fn retry_preserves_retained_foreign_entry_even_when_displaced_rename_is_blocked() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("subvol");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("workspace.txt"), "subvol-data").unwrap();
        let ws = tmp.path().join("workspace");
        symlink(&source, &ws).unwrap();

        let staging = make_staged(tmp.path(), "ws-abcdef");
        let staged_path = staging.path().to_path_buf();
        let blocked_target = displaced_path_for(&staged_path);
        // Occupy the `.ws-ckpt-displaced.*` target so `retain_displaced` must
        // keep the foreign directory under its original `.ws-ckpt-recover.*` name.
        fs::create_dir(&blocked_target).unwrap();
        let foreign = tmp.path().join("foreign-entry");
        fs::create_dir(&foreign).unwrap();
        fs::write(foreign.join("external.txt"), "still-must-survive").unwrap();
        let moved_away = tmp.path().join("moved-away");

        let mut calls = 0usize;
        let err = publish_with(staging, &ws, &source, |from, to, flags| {
            calls += 1;
            if calls == 1 {
                fs::remove_file(to).unwrap();
                fs::rename(&foreign, to).unwrap();
            } else if calls == 2 {
                fs::rename(to, &moved_away).unwrap();
            }
            renameat2(None, from, None, to, flags)
        })
        .unwrap_err();
        assert!(
            err.to_string().contains("displaced entry retained"),
            "{err:#}"
        );
        assert_eq!(
            fs::read_to_string(staged_path.join("external.txt")).unwrap(),
            "still-must-survive"
        );

        // Even though the foreign directory stayed under `.ws-ckpt-recover.*`,
        // its `.staging` marker was removed before EXCHANGE, so retry leaves it intact.
        restore("ws-abcdef", &source, &ws).await.unwrap();
        assert_eq!(
            fs::read_to_string(ws.join("workspace.txt")).unwrap(),
            "subvol-data"
        );
        assert_eq!(
            fs::read_to_string(staged_path.join("external.txt")).unwrap(),
            "still-must-survive"
        );
    }

    #[test]
    fn publish_fails_closed_on_enosys_and_einval() {
        for errno in [Errno::ENOSYS, Errno::EINVAL] {
            let tmp = tempfile::tempdir().unwrap();
            let source = tmp.path().join("subvol");
            fs::create_dir(&source).unwrap();
            let ws = tmp.path().join("workspace");
            symlink(&source, &ws).unwrap();

            let staging = make_staged(tmp.path(), "ws-abc123");
            let staged_path = staging.path().to_path_buf();
            let marker_path = staging.marker.clone();
            let err =
                publish_with(staging, &ws, &source, |_from, _to, _flags| Err(errno)).unwrap_err();

            assert!(
                err.to_string()
                    .contains("does not support atomic recovery publication"),
                "{errno}: {err:#}"
            );
            assert!(fs::symlink_metadata(&ws).unwrap().is_symlink());
            assert_eq!(ws.canonicalize().unwrap(), source.canonicalize().unwrap());
            assert!(!staged_path.exists(), "staging dir should be cleaned up");
            if let Some(marker) = marker_path {
                assert!(!marker.exists(), "staging marker should be cleaned up");
            }
        }
    }

    #[test]
    fn cleanup_stale_staging_matches_complete_id_and_verified_identity_only() {
        let tmp = tempfile::tempdir().unwrap();
        let supports_handles = inspect_dir(tmp.path()).unwrap().handle.is_some();

        // 1. Genuine crashed staging dir for `ws-abcdef` (with restrictive modes).
        let (stale_base, stale_base_marker) =
            abandon_as_crash(make_staged(tmp.path(), "ws-abcdef"));
        let nested = stale_base.join("ro-subdir");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("partial.txt"), "leftover").unwrap();
        fs::set_permissions(&nested, fs::Permissions::from_mode(0o555)).unwrap();
        fs::set_permissions(&stale_base, fs::Permissions::from_mode(0o555)).unwrap();

        // 2. Suffixed sibling workspace `ws-abcdef-2` must NOT match `ws-abcdef`.
        let (stale_suffixed, stale_suffixed_marker) =
            abandon_as_crash(make_staged(tmp.path(), "ws-abcdef-2"));
        fs::write(stale_suffixed.join("sibling.txt"), "keep-variant-2").unwrap();

        // 3. Another unrelated workspace `ws-def456`.
        let (other_ws, other_ws_marker) = abandon_as_crash(make_staged(tmp.path(), "ws-def456"));
        fs::write(other_ws.join("keep.txt"), "other").unwrap();

        // 4. Unmarked directory under `ws-abcdef`'s prefix must never be swept.
        let unmarked = tmp
            .path()
            .join(format!("{}unmarked", staging_dir_prefix("ws-abcdef")));
        fs::create_dir(&unmarked).unwrap();
        fs::write(unmarked.join("external.txt"), "unmarked-safe").unwrap();

        // 5. Different-inode replacement must never be deleted (only clear stale marker).
        let (replaced_dir, replaced_marker) =
            abandon_as_crash(make_staged(tmp.path(), "ws-abcdef"));
        let foreign_replacement = tmp.path().join("foreign-swap");
        fs::create_dir(&foreign_replacement).unwrap();
        fs::write(foreign_replacement.join("foreign.txt"), "wrong-inode").unwrap();
        fs::remove_dir_all(&replaced_dir).unwrap();
        fs::rename(&foreign_replacement, &replaced_dir).unwrap();

        // 6. Same-path delete+recreate (exercises real inode reuse on ext4/xfs)
        //    must preserve the replacement directory and clear the stale marker.
        let (recreated_dir, recreated_marker) =
            abandon_as_crash(make_staged(tmp.path(), "ws-abcdef"));
        fs::remove_dir_all(&recreated_dir).unwrap();
        fs::create_dir(&recreated_dir).unwrap();
        fs::write(recreated_dir.join("unrelated.txt"), "recreated-safe").unwrap();

        // 7. Explicit same-inode / stale-generation replacement (deterministic
        //    even on filesystems like tmpfs/btrfs that do not recycle inodes).
        let same_ino_dir = tmp
            .path()
            .join(format!("{}sameino1", staging_dir_prefix("ws-abcdef")));
        fs::create_dir(&same_ino_dir).unwrap();
        fs::write(same_ino_dir.join("precious.txt"), "same-inode-safe").unwrap();
        let same_ino_id = inspect_dir(&same_ino_dir).unwrap();
        let same_ino_marker = marker_path_for(&same_ino_dir).unwrap();
        let (stale_type, stale_bytes) = match &same_ino_id.handle {
            Some(handle) => {
                let mut mutated = handle.bytes.clone();
                if let Some(last) = mutated.last_mut() {
                    *last ^= 0xff;
                }
                (handle.handle_type, mutated)
            }
            None => (1, vec![0xde, 0xad, 0xbe, 0xef]),
        };
        fs::write(
            &same_ino_marker,
            format!(
                "{}\n{}\n{}:{}\n",
                encode_ws_id("ws-abcdef"),
                same_ino_id.ino,
                stale_type,
                hex::encode(&stale_bytes)
            ),
        )
        .unwrap();

        // 8. Legacy/incomplete inode-only marker (without file handle) must fail
        //    closed and preserve the directory.
        let legacy_dir = tmp
            .path()
            .join(format!("{}legacy01", staging_dir_prefix("ws-abcdef")));
        fs::create_dir(&legacy_dir).unwrap();
        fs::write(legacy_dir.join("keep.txt"), "legacy-marker-safe").unwrap();
        let legacy_meta = fs::symlink_metadata(&legacy_dir).unwrap();
        let legacy_marker = marker_path_for(&legacy_dir).unwrap();
        fs::write(
            &legacy_marker,
            format!("{}\n{}\n", encode_ws_id("ws-abcdef"), legacy_meta.ino()),
        )
        .unwrap();

        // 9. Symlink staging directory + marker must never be followed.
        let outside = tmp.path().join("outside-target");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("safe.txt"), "untouched").unwrap();
        let sym_dir = tmp
            .path()
            .join(format!("{}symlink1", staging_dir_prefix("ws-abcdef")));
        symlink(&outside, &sym_dir).unwrap();
        let sym_marker = marker_path_for(&sym_dir).unwrap();
        let outside_id = inspect_dir(&outside).unwrap();
        if let Some(handle) = &outside_id.handle {
            fs::write(
                &sym_marker,
                format!(
                    "{}\n{}\n{}:{}\n",
                    encode_ws_id("ws-abcdef"),
                    outside_id.ino,
                    handle.handle_type,
                    hex::encode(&handle.bytes)
                ),
            )
            .unwrap();
        }

        cleanup_stale_staging(tmp.path(), "ws-abcdef");

        if supports_handles {
            assert!(
                !stale_base.exists(),
                "matching crashed dir should be removed"
            );
            assert!(
                !stale_base_marker.as_ref().unwrap().exists(),
                "matching crashed marker should be removed"
            );
            assert!(stale_suffixed_marker.as_ref().unwrap().exists());
            assert!(other_ws_marker.as_ref().unwrap().exists());
            assert!(
                !replaced_marker.as_ref().unwrap().exists(),
                "different-inode marker should be cleared"
            );
            assert!(
                !recreated_marker.as_ref().unwrap().exists(),
                "recreated-dir marker should be cleared"
            );
            assert!(!sym_marker.exists(), "symlink marker should be cleared");
        } else {
            assert!(stale_base_marker.is_none());
            // Restore writable bits so tempdir cleanup succeeds on non-handle FS.
            make_dirs_writable(&stale_base);
        }

        assert_eq!(
            fs::read_to_string(stale_suffixed.join("sibling.txt")).unwrap(),
            "keep-variant-2"
        );
        assert_eq!(
            fs::read_to_string(other_ws.join("keep.txt")).unwrap(),
            "other"
        );
        assert_eq!(
            fs::read_to_string(unmarked.join("external.txt")).unwrap(),
            "unmarked-safe"
        );
        assert_eq!(
            fs::read_to_string(replaced_dir.join("foreign.txt")).unwrap(),
            "wrong-inode"
        );
        assert_eq!(
            fs::read_to_string(recreated_dir.join("unrelated.txt")).unwrap(),
            "recreated-safe"
        );
        assert_eq!(
            fs::read_to_string(same_ino_dir.join("precious.txt")).unwrap(),
            "same-inode-safe"
        );
        assert!(
            !same_ino_marker.exists(),
            "same-inode stale-handle marker should be cleared"
        );
        assert_eq!(
            fs::read_to_string(legacy_dir.join("keep.txt")).unwrap(),
            "legacy-marker-safe"
        );
        assert!(fs::symlink_metadata(&sym_dir).unwrap().is_symlink());
        assert_eq!(
            fs::read_to_string(outside.join("safe.txt")).unwrap(),
            "untouched"
        );

        // Sweeping `ws-abcdef-2` reclaims only its own crashed staging dir.
        cleanup_stale_staging(tmp.path(), "ws-abcdef-2");
        if supports_handles {
            assert!(!stale_suffixed.exists());
            assert!(!stale_suffixed_marker.as_ref().unwrap().exists());
        }
        assert_eq!(
            fs::read_to_string(other_ws.join("keep.txt")).unwrap(),
            "other"
        );
    }
}
