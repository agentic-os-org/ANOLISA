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

use std::fs::{File, Metadata};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::path::Path;

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
