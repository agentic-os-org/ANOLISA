//! Rooted file IO helpers backed by openat2(RESOLVE_BENEATH|RESOLVE_NO_SYMLINKS).
//!
//! `ns::paths::resolve_path` validates a string path is sandbox-safe AT
//! CHECK TIME, but between the check and the subsequent `fs::*` call an
//! attacker with write access to the mount tree could swap a component
//! for a symlink and escape — e.g. swap `notes/x` for a link to
//! `~/.ssh/id_rsa`, then have the model do `mem_read("notes/x")`.
//!
//! Tier A tools that open file content route through this module: every
//! open targets the mount's `root_fd` (opened once at startup with
//! O_PATH) and the kernel refuses to traverse `..` or any symlink. For
//! tools that don't open file contents (mkdir, remove, list traversal),
//! `assert_no_symlink_traversal` validates the resolved path doesn't
//! cross a symlink before the syscall — best-effort but closes the
//! common-case attack.
//!
//! Linux-only (the parent crate already is). Requires kernel ≥ 5.6 for
//! openat2 + ResolveFlag; AOS ships 6.x.

use std::ffi::{CString, OsStr};
use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

use nix::fcntl::{OFlag, OpenHow, ResolveFlag, open, openat2};
use nix::sys::stat::Mode;

use crate::error::{MemoryError, Result};

/// Sandbox flags applied to every openat2 call: refuse to leave the root
/// (BENEATH) and refuse to follow ANY symlink on the way (NO_SYMLINKS).
fn safe_resolve() -> ResolveFlag {
    ResolveFlag::RESOLVE_BENEATH | ResolveFlag::RESOLVE_NO_SYMLINKS
}

/// Open the mount root for use as the `dirfd` of subsequent openat2
/// calls. `O_PATH` keeps the cost minimal — we don't read through it
/// directly, only resolve children against it.
pub fn open_root(path: &Path) -> Result<OwnedFd> {
    let raw = open(
        path,
        OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )
    .map_err(|e| MemoryError::Other(format!("open root {}: {e}", path.display())))?;
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// openat2 itself returns a RawFd (nix 0.29); wrap it in an OwnedFd so
/// the drop closes the descriptor and we don't leak on early return.
fn openat2_owned(root: BorrowedFd<'_>, rel: &Path, how: OpenHow) -> Result<OwnedFd> {
    let raw = openat2(root.as_raw_fd(), rel, how).map_err(|e| translate_open_error(rel, e))?;
    Ok(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Resolve `rel` against `root` with sandbox flags, opening for the
/// requested access. The returned `File` borrows nothing from `root`;
/// dropping it closes the underlying fd.
fn open_in_root(root: BorrowedFd<'_>, rel: &Path, flags: OFlag, mode: Mode) -> Result<File> {
    let how = OpenHow::new()
        .flags(flags | OFlag::O_CLOEXEC)
        .mode(mode)
        .resolve(safe_resolve());
    let owned = openat2_owned(root, rel, how)?;
    Ok(File::from(owned))
}

fn translate_open_error(rel: &Path, e: nix::errno::Errno) -> MemoryError {
    use nix::errno::Errno;
    match e {
        Errno::ENOENT => MemoryError::NotFound(rel.display().to_string()),
        Errno::EEXIST => MemoryError::AlreadyExists(rel.display().to_string()),
        // ELOOP / EXDEV / E2BIG are what the kernel uses to signal a
        // resolve constraint was hit (symlink, mount crossing, etc).
        // Map them to PathOutsideMount so the caller's audit log makes
        // the security intent obvious.
        Errno::ELOOP | Errno::EXDEV => MemoryError::PathOutsideMount(rel.display().to_string()),
        other => MemoryError::Other(format!("openat2 {}: {other}", rel.display())),
    }
}

pub fn read_to_string(root: BorrowedFd<'_>, rel: &Path) -> Result<String> {
    let mut f = open_in_root(root, rel, OFlag::O_RDONLY, Mode::empty())?;
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
}

/// Open a file for streaming read. Used by grep so we can iterate lines
/// without buffering the whole file.
pub fn open_read(root: BorrowedFd<'_>, rel: &Path) -> Result<File> {
    open_in_root(root, rel, OFlag::O_RDONLY, Mode::empty())
}

/// Write the file's full content. Creates if missing; truncates if
/// present. Use `write_create_new` when `overwrite=false` semantics are
/// required.
pub fn write(root: BorrowedFd<'_>, rel: &Path, content: &[u8]) -> Result<u64> {
    let mut f = open_in_root(
        root,
        rel,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_TRUNC,
        Mode::from_bits_truncate(0o644),
    )?;
    f.write_all(content)?;
    f.flush()?;
    Ok(content.len() as u64)
}

/// Write only if the file doesn't exist; fails with `AlreadyExists`
/// otherwise. This is the create-new semantic mem_write wants when
/// `overwrite=false`.
pub fn write_create_new(root: BorrowedFd<'_>, rel: &Path, content: &[u8]) -> Result<u64> {
    let mut f = open_in_root(
        root,
        rel,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL,
        Mode::from_bits_truncate(0o644),
    )?;
    f.write_all(content)?;
    f.flush()?;
    Ok(content.len() as u64)
}

pub fn append(root: BorrowedFd<'_>, rel: &Path, content: &[u8]) -> Result<u64> {
    let mut f = open_in_root(
        root,
        rel,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_APPEND,
        Mode::from_bits_truncate(0o644),
    )?;
    f.write_all(content)?;
    f.flush()?;
    Ok(content.len() as u64)
}

/// `stat`-equivalent that refuses to traverse symlinks. Returns
/// `NotFound` if the path doesn't exist, `PathOutsideMount` if a
/// component is a symlink.
pub fn metadata(root: BorrowedFd<'_>, rel: &Path) -> Result<Metadata> {
    let how = OpenHow::new()
        .flags(OFlag::O_PATH | OFlag::O_CLOEXEC)
        .resolve(safe_resolve());
    let owned = openat2_owned(root, rel, how)?;
    let f = File::from(owned);
    Ok(f.metadata()?)
}

pub fn exists(root: BorrowedFd<'_>, rel: &Path) -> bool {
    metadata(root, rel).is_ok()
}

// ---- Directory-scoped IO -------------------------------------------------
//
// The helpers below anchor every operation to a descriptor the kernel has
// already resolved beneath the mount root with RESOLVE_NO_SYMLINKS, and the
// mutating ones (`mkdirat` / `renameat` / `unlinkat` / `fstatat`) never
// re-walk a path at all — so a component swapped between two calls cannot
// redirect them. `mem_import`'s pre-overwrite backup is the caller that
// needs this: it deletes the whole store on the strength of that backup
// being durable and in-mount, so both the escape (a symlinked
// `.anolisa/backups`) and the lost dirent (a parent never fsynced) failure
// modes end in data loss.

/// Open a directory beneath `root` as an `O_RDONLY` descriptor: usable as
/// the `dirfd` of the `*at` calls below and as an `fsync` target, which the
/// `O_PATH` descriptor `open_root` hands back is not — `fsync` on `O_PATH`
/// is `EBADF`. Pass `"."` for a syncable descriptor on `root` itself.
///
/// Fails with `PathOutsideMount` when any component is a symlink.
pub fn open_dir(root: BorrowedFd<'_>, rel: &Path) -> Result<File> {
    open_in_root(
        root,
        rel,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY,
        Mode::empty(),
    )
}

/// `create_dir_all` for a rooted path, with the two properties the `std::fs`
/// version cannot give: no component is traversed without the kernel
/// refusing symlinks, and the parent of every directory this call creates is
/// fsynced.
///
/// That fsync is not decoration. The dirent naming a new directory lives in
/// its *parent*, so a crash right after `create_dir_all` returns can drop
/// the directory and the file just written into it, however carefully that
/// file was synced itself — which for a pre-overwrite backup means the store
/// is deleted against a recovery path that no longer exists.
///
/// Idempotent: an existing directory is not an error. Everything else is — a
/// symlinked or non-directory component comes back as `PathOutsideMount` /
/// `Other` rather than a quiet success, because a caller that destroys data
/// on `Ok` has to be able to trust it.
pub fn create_dir_durable(root: BorrowedFd<'_>, rel: &Path) -> Result<()> {
    use std::path::Component;

    // Opened up front so even a first-level mkdir has an fsync-able parent;
    // `root` itself is O_PATH.
    let mut parent = open_dir(root, Path::new("."))?;
    let mut probe = PathBuf::new();

    for comp in rel.components() {
        let seg = match comp {
            Component::Normal(s) => s,
            _ => return Err(MemoryError::PathOutsideMount(rel.display().to_string())),
        };
        probe.push(seg);

        match mkdirat(parent.as_fd(), seg) {
            Ok(()) => parent.sync_all()?,
            // Already present. The `open_dir` below is what proves it is a
            // real directory and not a symlink or a regular file.
            Err(nix::errno::Errno::EEXIST) => {}
            Err(e) => {
                return Err(MemoryError::Other(format!(
                    "mkdirat {}: {e}",
                    probe.display()
                )));
            }
        }

        // Re-resolve from `root` instead of keeping the descriptor the mkdir
        // was issued against: this one was validated with
        // RESOLVE_NO_SYMLINKS *after* the mkdir, so a swap in between cannot
        // hand the next level a redirected parent.
        parent = open_dir(root, &probe)?;
    }
    Ok(())
}

/// `mkdirat(2)` against a descriptor. nix 0.29 wraps `mkdir` but not
/// `mkdirat`, and the path-based form is exactly what this module exists to
/// avoid.
fn mkdirat(dir: BorrowedFd<'_>, seg: &OsStr) -> std::result::Result<(), nix::errno::Errno> {
    use std::os::unix::ffi::OsStrExt;

    let name = match CString::new(seg.as_bytes()) {
        Ok(c) => c,
        // An interior NUL can never name a file; EINVAL is what the kernel
        // itself reports for a malformed name.
        Err(_) => return Err(nix::errno::Errno::EINVAL),
    };
    let rc = unsafe {
        nix::libc::mkdirat(
            dir.as_raw_fd(),
            name.as_ptr(),
            Mode::from_bits_truncate(0o755).bits(),
        )
    };
    nix::errno::Errno::result(rc).map(drop)
}

/// Create a new regular file inside an already-rooted directory. `O_EXCL` so
/// a name that is taken fails instead of being written through — a planted
/// symlink answers `EEXIST` here even before RESOLVE_NO_SYMLINKS does.
pub fn create_new_in_dir(dir: BorrowedFd<'_>, name: &Path) -> Result<File> {
    open_in_root(
        dir,
        name,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL,
        Mode::from_bits_truncate(0o644),
    )
}

/// `renameat(2)` within one rooted directory: both names resolve against the
/// same descriptor, so the rename can neither escape it nor be redirected by
/// a parent swapped between the write and the rename.
pub fn rename_in_dir(dir: BorrowedFd<'_>, from: &Path, to: &Path) -> Result<()> {
    nix::fcntl::renameat(Some(dir.as_raw_fd()), from, Some(dir.as_raw_fd()), to).map_err(|e| {
        MemoryError::Other(format!(
            "renameat {} -> {}: {e}",
            from.display(),
            to.display()
        ))
    })
}

/// Names of the regular files directly inside `dir`, enumerated through the
/// descriptor (`fdopendir`) and classified with `fstatat` +
/// `AT_SYMLINK_NOFOLLOW`. Anything that is not a regular file — a
/// subdirectory, a planted symlink — is left out, so a caller that prunes by
/// name can neither count nor delete something it does not own.
pub fn regular_file_names(dir: &File) -> Result<Vec<String>> {
    // fdopendir takes ownership of the descriptor it is handed, so give it a
    // dup and keep the original for the fstatat calls below.
    let dup = dir.try_clone()?;
    let mut handle =
        nix::dir::Dir::from(dup).map_err(|e| MemoryError::Other(format!("fdopendir: {e}")))?;

    let mut names = Vec::new();
    for entry_res in handle.iter() {
        let entry = entry_res.map_err(|e| MemoryError::Other(format!("readdir: {e}")))?;
        let name = entry.file_name();
        let bytes = name.to_bytes();
        if bytes == b"." || bytes == b".." {
            continue;
        }
        let stat = nix::sys::stat::fstatat(
            Some(dir.as_raw_fd()),
            name,
            nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
        )
        .map_err(|e| MemoryError::Other(format!("fstatat {}: {e}", name.to_string_lossy())))?;
        if stat.st_mode & nix::libc::S_IFMT == nix::libc::S_IFREG {
            names.push(String::from_utf8_lossy(bytes).into_owned());
        }
    }
    Ok(names)
}

/// `unlinkat(2)` for a regular file inside an already-rooted directory. The
/// name is looked up against `dir`, so a parent swapped for a symlink after
/// `dir` was opened cannot send the delete somewhere else.
pub fn unlink_in_dir(dir: BorrowedFd<'_>, name: &str) -> Result<()> {
    nix::unistd::unlinkat(
        Some(dir.as_raw_fd()),
        name,
        nix::unistd::UnlinkatFlags::NoRemoveDir,
    )
    .map_err(|e| MemoryError::Other(format!("unlinkat {name}: {e}")))
}

/// Reject paths under a `.git/` directory at the mount root. Git internal
/// files (HEAD, refs, COMMIT_EDITMSG, logs/) are OS-managed, not user
/// memory, and must be excluded from indexing and context assembly just
/// like the `.anolisa/` meta dir. Shared by the index worker and
/// `memory_get_context` so the reserved-path set stays consistent.
pub fn is_under_git(path: &Path, root: &Path) -> bool {
    path.strip_prefix(root)
        .ok()
        .and_then(|rel| rel.components().next())
        .map(|c| c.as_os_str() == ".git")
        .unwrap_or(false)
}

/// Probe a path to confirm no symlink lies anywhere on the resolution
/// path. Used by `mkdir` / `remove` (which still go through `std::fs`
/// because openat2 has no recursive-rm primitive) to short-circuit
/// symlink attacks before they reach the unsandboxed syscall.
///
/// If the path doesn't exist yet, walks the longest existing prefix.
pub fn assert_no_symlink_traversal(root: BorrowedFd<'_>, rel: &Path) -> Result<()> {
    use std::path::Component;

    let mut probe = std::path::PathBuf::new();
    for comp in rel.components() {
        let seg = match comp {
            Component::Normal(s) => s,
            _ => return Err(MemoryError::PathOutsideMount(rel.display().to_string())),
        };
        probe.push(seg);
        let how = OpenHow::new()
            .flags(OFlag::O_PATH | OFlag::O_CLOEXEC)
            .resolve(safe_resolve());
        match openat2(root.as_raw_fd(), probe.as_path(), how) {
            Ok(raw_fd) => {
                // Wrap so Drop closes the path fd before next iter.
                let _owned = unsafe { OwnedFd::from_raw_fd(raw_fd) };
            }
            Err(nix::errno::Errno::ENOENT) => {
                // This component doesn't exist — the rest of the path is
                // therefore "fresh", nothing more to validate.
                return Ok(());
            }
            Err(e) => return Err(translate_open_error(&probe, e)),
        }
    }
    Ok(())
}

/// Recursively remove a directory, refusing to follow any symlink found
/// inside it. `std::fs::remove_dir_all` follows symlinks, which means a
/// symlink inside the target dir pointing outside the mount would destroy
/// the link target. This function instead:
/// 1. Walks directory contents using `openat2` (RESOLVE_BENEATH) to open
///    each entry, so symlink traversal is blocked at kernel level.
/// 2. For each entry, checks if it's a symlink → reject with `PathOutsideMount`.
/// 3. For files, deletes via `std::fs::remove_file` on the resolved path.
/// 4. For directories, recurses.
/// 5. Finally removes the now-empty top-level directory.
pub fn remove_dir_all_safe(root: BorrowedFd<'_>, rel: &Path, abs: &Path) -> Result<()> {
    remove_dir_all_recursive(root, rel, abs)?;
    std::fs::remove_dir(abs)?;
    Ok(())
}

/// Precondition invariant: no symlink should exist inside the mount at
/// any time. The model has no symlink creation primitive, and any path
/// capable of introducing symlinks (snapshot restore, git checkout) must
/// filter them at its own entry point before content reaches the mount.
///
/// TOCTOU hardening: dirent enumeration is anchored to a kernel fd from
/// `openat2` (RESOLVE_BENEATH | RESOLVE_NO_SYMLINKS) — never the absolute
/// path — so symlink swaps between probe and removal are impossible.
/// `fdopendir` lists names; `fstatat(parent_fd, name, AT_SYMLINK_NOFOLLOW)`
/// classifies each entry without traversing symlinks; `unlinkat` removes
/// by parent-fd + name with no path re-resolution.
fn remove_dir_all_recursive(root: BorrowedFd<'_>, rel: &Path, abs: &Path) -> Result<()> {
    use std::os::unix::ffi::OsStrExt;

    // Open the parent directory as O_RDONLY so we can fdopendir it.
    // O_PATH cannot be used as the dirfd of fdopendir(3) on Linux.
    let parent_how = OpenHow::new()
        .flags(OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC)
        .resolve(safe_resolve());
    let parent_fd = openat2_owned(root, rel, parent_how)?;

    // Snapshot dirents into a Vec so we can drop the Dir handle (and its
    // dirent buffer) before recursing. Without this, deep trees keep one
    // Dir open per stack frame and can exhaust RLIMIT_NOFILE.
    let entries: Vec<(std::ffi::OsString, nix::libc::mode_t)> = {
        // fdopendir consumes the fd, so dup it: we still need parent_fd
        // as the anchor for fstatat / unlinkat below.
        let dir_fd = parent_fd
            .try_clone()
            .map_err(|e| MemoryError::Other(format!("dup parent fd {}: {e}", rel.display())))?;
        let mut dir = nix::dir::Dir::from(dir_fd)
            .map_err(|e| MemoryError::Other(format!("fdopendir {}: {e}", rel.display())))?;

        let mut out = Vec::new();
        for entry_res in dir.iter() {
            let entry = entry_res
                .map_err(|e| MemoryError::Other(format!("readdir {}: {e}", rel.display())))?;
            let name_bytes = entry.file_name().to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }
            let name_os = std::ffi::OsStr::from_bytes(name_bytes).to_os_string();

            // Classify via fstatat anchored at parent_fd, never via path.
            // AT_SYMLINK_NOFOLLOW: lstat semantics — never traverses a link.
            let stat = nix::sys::stat::fstatat(
                Some(parent_fd.as_raw_fd()),
                name_os.as_os_str(),
                nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW,
            )
            .map_err(|e| {
                MemoryError::Other(format!("fstatat {}: {e}", rel.join(&name_os).display()))
            })?;
            out.push((name_os, stat.st_mode));
        }
        out
        // `dir` drops here, releasing the dup'd fd before we recurse.
    };

    for (name_os, mode) in entries {
        let child_rel = rel.join(&name_os);
        let ifmt = mode & nix::libc::S_IFMT;

        if ifmt == nix::libc::S_IFLNK {
            return Err(MemoryError::PathOutsideMount(
                child_rel.display().to_string(),
            ));
        }
        if ifmt == nix::libc::S_IFDIR {
            let child_abs = abs.join(&name_os);
            remove_dir_all_recursive(root, &child_rel, &child_abs)?;
            nix::unistd::unlinkat(
                Some(parent_fd.as_raw_fd()),
                name_os.as_os_str(),
                nix::unistd::UnlinkatFlags::RemoveDir,
            )
            .map_err(|e| {
                MemoryError::Other(format!("unlinkat dir {}: {e}", child_rel.display()))
            })?;
        } else {
            nix::unistd::unlinkat(
                Some(parent_fd.as_raw_fd()),
                name_os.as_os_str(),
                nix::unistd::UnlinkatFlags::NoRemoveDir,
            )
            .map_err(|e| {
                MemoryError::Other(format!("unlinkat file {}: {e}", child_rel.display()))
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsFd;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    #[test]
    fn read_write_roundtrip() {
        let tmp = tempdir().unwrap();
        let root = open_root(tmp.path()).unwrap();
        write(root.as_fd(), Path::new("a.md"), b"hello").unwrap();
        assert_eq!(
            read_to_string(root.as_fd(), Path::new("a.md")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn write_create_new_refuses_existing() {
        let tmp = tempdir().unwrap();
        let root = open_root(tmp.path()).unwrap();
        write_create_new(root.as_fd(), Path::new("a.md"), b"v1").unwrap();
        let err = write_create_new(root.as_fd(), Path::new("a.md"), b"v2").unwrap_err();
        assert!(matches!(err, MemoryError::AlreadyExists(_)));
    }

    #[test]
    fn read_refuses_symlink_target() {
        let tmp = tempdir().unwrap();
        let outside = tempdir().unwrap();
        let secret = outside.path().join("secret.txt");
        std::fs::write(&secret, "TOP_SECRET").unwrap();

        let root = open_root(tmp.path()).unwrap();
        symlink(&secret, tmp.path().join("leak")).unwrap();

        let err = read_to_string(root.as_fd(), Path::new("leak")).unwrap_err();
        assert!(
            matches!(err, MemoryError::PathOutsideMount(_)),
            "expected PathOutsideMount, got {err:?}"
        );
    }

    #[test]
    fn write_refuses_symlink_parent() {
        let tmp = tempdir().unwrap();
        let outside = tempdir().unwrap();
        std::fs::create_dir(outside.path().join("victim")).unwrap();

        let root = open_root(tmp.path()).unwrap();
        symlink(outside.path().join("victim"), tmp.path().join("dir")).unwrap();

        let err = write(root.as_fd(), Path::new("dir/file.md"), b"escape").unwrap_err();
        assert!(matches!(err, MemoryError::PathOutsideMount(_)));
    }

    #[test]
    fn parent_dotdot_is_refused() {
        let tmp = tempdir().unwrap();
        let root = open_root(tmp.path()).unwrap();
        // openat2 with BENEATH refuses any path containing `..`.
        let err = read_to_string(root.as_fd(), Path::new("../etc/passwd")).unwrap_err();
        assert!(
            matches!(
                err,
                MemoryError::PathOutsideMount(_) | MemoryError::Other(_)
            ),
            "got {err:?}"
        );
    }

    #[test]
    fn assert_no_symlink_traversal_passes_for_normal_paths() {
        let tmp = tempdir().unwrap();
        let root = open_root(tmp.path()).unwrap();
        std::fs::create_dir(tmp.path().join("notes")).unwrap();
        std::fs::write(tmp.path().join("notes/x.md"), "x").unwrap();
        assert!(assert_no_symlink_traversal(root.as_fd(), Path::new("notes/x.md")).is_ok());
        // Non-existing leaf is OK (mkdir/write target before creation).
        assert!(assert_no_symlink_traversal(root.as_fd(), Path::new("notes/new.md")).is_ok());
    }

    #[test]
    fn assert_no_symlink_traversal_catches_symlink_dir() {
        let tmp = tempdir().unwrap();
        let outside = tempdir().unwrap();
        symlink(outside.path(), tmp.path().join("link")).unwrap();
        let root = open_root(tmp.path()).unwrap();
        let err = assert_no_symlink_traversal(root.as_fd(), Path::new("link/file.md")).unwrap_err();
        assert!(matches!(err, MemoryError::PathOutsideMount(_)));
    }
}
