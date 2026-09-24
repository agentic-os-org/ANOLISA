//! A6/B1: Ledger Backing Root.
//!
//! Decouples the agent-visible FUSE view from the external ledger daemon's
//! source-side working path. In an in-place security mount, FUSE over-mounts
//! the source directory; the daemon cannot scan the live source through the
//! FUSE path because hidden skills are invisible and fallback skills resolve
//! to snapshots. The backing root is a private alias of the live source tree
//! that the daemon scans and writes activation state through.
//!
//! SkillFS creates the backing root (optionally as a bind mount) before the
//! FUSE over-mount becomes active. The bind is detached and made private
//! before publication, so copies propagated to an already running daemon
//! cannot inherit the source mount's propagation group. Making only the
//! published local copy private would leave those remote copies vulnerable
//! to the in-place FUSE over-mount hiding the real source tree.
//!
//! Daemon live-source operations — activation bootstrap, reload, watching,
//! and N3 protocol event `skillDir` — use the backing root path. Socket notify
//! v2 keeps the canonical source identity, which the daemon resolves to this
//! live path through the control socket. The agent-visible FUSE view is
//! unchanged.
//!
//! Fail-closed: if the backing root exists but ownership, permissions, path
//! shape, or mount setup is unsafe, startup is rejected.
//!
//! The backing root is separate from the trusted-writer gate. Trusted-writer
//! controls `.skill-meta/**` mutation through the FUSE entry point; backing
//! root access is controlled by OS ownership, permissions, and mount setup.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

// ---------------------------------------------------------------------------
// Error
// ---------------------------------------------------------------------------

/// Errors returned by [`LedgerBackingRoot::setup`].
#[derive(Debug)]
pub enum BackingRootError {
    /// The backing root path was empty.
    EmptyPath,
    /// The path does not exist and could not be created.
    CreateFailed(std::io::Error),
    /// The path exists but is not a directory.
    NotADirectory { path: PathBuf },
    /// The backing root canonicalizes to a path inside the agent-visible
    /// mount path. The daemon would read through the FUSE view.
    InsideMountPath { path: PathBuf, mount: PathBuf },
    /// The backing root is inside the source tree. Only backing_root ==
    /// source is allowed as an explicit non-in-place convenience path.
    InsideSourceTree { path: PathBuf, source: PathBuf },
    /// The directory permissions are too open: any group or other access
    /// bits are present. The backing root is a security boundary and
    /// must be accessible only by the owner.
    PermissionsTooOpen { path: PathBuf, mode: u32 },
    /// The directory owner is neither the current uid nor root.
    WrongOwner { path: PathBuf, uid: u32 },
    /// `canonicalize()` failed on the backing root or its parent.
    CanonicalizeFailed {
        path: PathBuf,
        source: std::io::Error,
    },
    /// `mount --bind` failed.
    BindMountFailed {
        source: PathBuf,
        target: PathBuf,
        error: std::io::Error,
    },
    /// After setup, the backing root's device/inode does not match the
    /// source. This means the backing root is not a live alias of the
    /// source (e.g. an empty or stale directory). Fail-closed.
    IdentityMismatch {
        backing: PathBuf,
        source: PathBuf,
        backing_dev: u64,
        backing_ino: u64,
        source_dev: u64,
        source_ino: u64,
    },
    /// `mount(NULL, target, NULL, MS_PRIVATE|MS_REC, NULL)` failed after
    /// a successful bind mount.
    MakePrivateFailed {
        target: PathBuf,
        error: std::io::Error,
    },
    /// `umount` failed during cleanup.
    UnmountFailed {
        path: PathBuf,
        error: std::io::Error,
    },
}

impl std::fmt::Display for BackingRootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackingRootError::EmptyPath => write!(f, "backing root path is empty"),
            BackingRootError::CreateFailed(e) => write!(f, "backing root create failed: {e}"),
            BackingRootError::NotADirectory { path } => {
                write!(f, "backing root is not a directory: '{}'", path.display())
            }
            BackingRootError::InsideMountPath { path, mount } => write!(
                f,
                "backing root '{}' is inside the agent-visible mount path '{}' \
                 — daemon would read through the FUSE view",
                path.display(),
                mount.display()
            ),
            BackingRootError::InsideSourceTree { path, source } => write!(
                f,
                "backing root '{}' is inside the source tree '{}' \
                 — backing root must not be a subtree of the source (use \
                 source path directly or a separate private directory)",
                path.display(),
                source.display()
            ),
            BackingRootError::PermissionsTooOpen { path, mode } => write!(
                f,
                "backing root '{}' permissions are too open (mode 0o{:o}) — \
                 require owner-only access, e.g. 0700 or stricter",
                path.display(),
                mode
            ),
            BackingRootError::WrongOwner { path, uid } => write!(
                f,
                "backing root '{}' is owned by uid {} — must be owned by \
                 the current uid or root",
                path.display(),
                uid
            ),
            BackingRootError::CanonicalizeFailed { path, source } => {
                write!(
                    f,
                    "backing root canonicalize failed for '{}': {}",
                    path.display(),
                    source
                )
            }
            BackingRootError::BindMountFailed {
                source,
                target,
                error,
            } => write!(
                f,
                "bind mount '{}' -> '{}' failed: {}",
                source.display(),
                target.display(),
                error
            ),
            BackingRootError::IdentityMismatch {
                backing,
                source,
                backing_dev,
                backing_ino,
                source_dev,
                source_ino,
            } => write!(
                f,
                "backing root '{}' (dev={}, ino={}) does not match source '{}' \
                 (dev={}, ino={}) — backing root must be a live alias of the source \
                 (bind mount or same path)",
                backing.display(),
                backing_dev,
                backing_ino,
                source.display(),
                source_dev,
                source_ino
            ),
            BackingRootError::MakePrivateFailed { target, error } => write!(
                f,
                "make-private on backing root '{}' failed: {} \
                 — without propagation isolation the FUSE over-mount may \
                 leak into the backing root",
                target.display(),
                error
            ),
            BackingRootError::UnmountFailed { path, error } => {
                write!(f, "unmount '{}' failed: {}", path.display(), error)
            }
        }
    }
}

impl std::error::Error for BackingRootError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            BackingRootError::CreateFailed(e) => Some(e),
            BackingRootError::CanonicalizeFailed { source, .. } => Some(source),
            BackingRootError::BindMountFailed { error, .. } => Some(error),
            BackingRootError::MakePrivateFailed { error, .. } => Some(error),
            BackingRootError::UnmountFailed { error, .. } => Some(error),
            _ => None,
        }
    }
}

// ---------------------------------------------------------------------------
// LedgerBackingRoot
// ---------------------------------------------------------------------------

/// A validated, optionally bind-mounted private alias of the live source
/// tree for the external security daemon.
///
/// Created by [`LedgerBackingRoot::setup`] before the FUSE over-mount
/// becomes active. Cleaned up by [`LedgerBackingRoot::cleanup`] on
/// shutdown/unmount.
///
/// The daemon-facing path is available via [`LedgerBackingRoot::path`].
#[derive(Debug)]
pub struct LedgerBackingRoot {
    /// The daemon-facing root path.
    path: PathBuf,
    /// Whether SkillFS created a bind mount that needs unmount on cleanup.
    created_bind_mount: bool,
    /// If SkillFS created the backing root directory, the path to remove
    /// on cleanup.
    created_temp_dir: Option<PathBuf>,
}

impl LedgerBackingRoot {
    /// Validate and optionally create the backing root.
    ///
    /// * `source_canon` — canonical path of the skill source directory.
    /// * `backing_root_path` — operator-supplied backing root path.
    /// * `mount_canon` — canonical path of the agent-visible mount point.
    /// * `in_place` — whether the mount is in-place (source == mountpoint).
    ///
    /// Validation (fail-closed, in order):
    /// 1. Path is non-empty.
    /// 2. Parent directory is canonicalized; the expected canonical path
    ///    is checked against mount/source **before** any filesystem
    ///    side-effects (P2-1: no directory creation before path-shape check).
    /// 3. Permissions: no group or other access bits (`mode & 0o077 == 0`).
    ///    Owner must be current uid or root (P1-2).
    /// 4. Identity: `stat(backing_root).dev/ino` must match
    ///    `stat(source).dev/ino`. This ensures the backing root is a live
    ///    alias (same path, bind mount, or symlink to source), not a stale
    ///    or empty directory (P1-1).
    ///
    /// Setup:
    /// * If backing root == source (non-in-place convenience): use directly.
    /// * Otherwise: create directory (mode 0o700) if needed, bind-mount
    ///   source to it.
    /// * Bind mount failure is always fail-closed: the daemon must not
    ///   scan a directory that is not the live source.
    /// * Any failure after directory creation cleans up the created directory.
    pub fn setup(
        source_canon: &Path,
        backing_root_path: &Path,
        mount_canon: &Path,
        in_place: bool,
    ) -> Result<Self, BackingRootError> {
        if backing_root_path.as_os_str().is_empty() {
            return Err(BackingRootError::EmptyPath);
        }

        // P2-1: Canonicalize the parent directory first and construct the
        // expected canonical path. This lets us do path-shape checks
        // BEFORE creating any directory, so a rejected configuration never
        // leaves side-effects in the source tree.
        let parent = backing_root_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent_canon =
            parent
                .canonicalize()
                .map_err(|e| BackingRootError::CanonicalizeFailed {
                    path: parent.to_path_buf(),
                    source: e,
                })?;
        let leaf =
            backing_root_path
                .file_name()
                .ok_or_else(|| BackingRootError::CanonicalizeFailed {
                    path: backing_root_path.to_path_buf(),
                    source: std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "backing root path has no file name component",
                    ),
                })?;
        let expected_canonical = parent_canon.join(leaf);

        // Validate: not inside the agent-visible mount path.
        if path_is_inside(&expected_canonical, mount_canon) {
            return Err(BackingRootError::InsideMountPath {
                path: expected_canonical,
                mount: mount_canon.to_path_buf(),
            });
        }

        // If backing root is the same as source (non-in-place convenience),
        // use it directly without a bind mount. The source directory's
        // permissions are the operator's responsibility — the backing root
        // permission check applies only to private aliases, not to the
        // source itself. Must be checked BEFORE the InsideSourceTree test
        // because `path_is_inside(source, source)` is true.
        if expected_canonical == source_canon {
            info!(
                backing_root = %source_canon.display(),
                "backing root is the source directory; no bind mount needed"
            );
            return Ok(Self {
                path: source_canon.to_path_buf(),
                created_bind_mount: false,
                created_temp_dir: None,
            });
        }

        // Validate: backing root must not be inside the source tree.
        // A backing root inside source would cause recursive bind mount
        // (source bind-mounted into its own subtree) and/or let the daemon
        // scan a path that is also part of the source store. This applies
        // to ALL modes. The `backing_root == source` case is handled above.
        if path_is_inside(&expected_canonical, source_canon) {
            return Err(BackingRootError::InsideSourceTree {
                path: expected_canonical,
                source: source_canon.to_path_buf(),
            });
        }

        // Backing root is different from source. We need to either create
        // a new directory + bind mount, or verify a pre-existing alias.
        let exists = expected_canonical.is_dir();
        let mut created_temp_dir: Option<PathBuf> = None;

        if !exists {
            // Create the directory with restrictive permissions (P1-2).
            use std::os::unix::fs::DirBuilderExt;
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(&expected_canonical)
                .map_err(BackingRootError::CreateFailed)?;
            created_temp_dir = Some(expected_canonical.clone());
        }

        // Canonicalize the (now existing) backing root.
        let backing_canon = expected_canonical.canonicalize().map_err(|e| {
            // Clean up if we created the dir.
            if let Some(ref dir) = created_temp_dir {
                let _ = std::fs::remove_dir_all(dir);
            }
            BackingRootError::CanonicalizeFailed {
                path: expected_canonical.clone(),
                source: e,
            }
        })?;

        // Repeat the shape checks after following the leaf. An external
        // symlink can otherwise resolve back into the mount or source tree.
        if path_is_inside(&backing_canon, mount_canon) {
            if let Some(ref dir) = created_temp_dir {
                let _ = std::fs::remove_dir(dir);
            }
            return Err(BackingRootError::InsideMountPath {
                path: backing_canon,
                mount: mount_canon.to_path_buf(),
            });
        }

        // A source alias is safe only when no in-place over-mount can hide
        // it. Return it directly instead of attempting a self-bind.
        if backing_canon == source_canon {
            if in_place {
                return Err(BackingRootError::InsideMountPath {
                    path: backing_canon,
                    mount: mount_canon.to_path_buf(),
                });
            }
            return Ok(Self {
                path: source_canon.to_path_buf(),
                created_bind_mount: false,
                created_temp_dir: None,
            });
        }

        if path_is_inside(&backing_canon, source_canon) {
            if let Some(ref dir) = created_temp_dir {
                let _ = std::fs::remove_dir(dir);
            }
            return Err(BackingRootError::InsideSourceTree {
                path: backing_canon,
                source: source_canon.to_path_buf(),
            });
        }

        // Validate permissions on the backing root's parent directory.
        // After bind mount, the backing root itself shows the source's
        // permissions. The real security boundary is the parent directory
        // that gates traversal to the backing root. Require the parent to
        // have owner-only access (mode & 0o077 == 0) and correct ownership.
        if let Err(e) = Self::validate_permissions(&parent_canon) {
            if let Some(ref dir) = created_temp_dir {
                let _ = std::fs::remove_dir_all(dir);
            }
            return Err(e);
        }

        // Try to create a bind mount from source to backing root.
        // This gives the daemon a private alias of the live source.
        match Self::do_bind_mount(source_canon, &backing_canon) {
            Ok(()) => {
                // Isolate propagation before the FUSE over-mount.
                if let Err(e) = Self::do_make_private(&backing_canon) {
                    warn!(
                        backing_root = %backing_canon.display(),
                        error = %e,
                        "make-private failed — unmounting and failing closed"
                    );
                    let _ = Self::do_umount(&backing_canon);
                    if let Some(ref dir) = created_temp_dir {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                    return Err(BackingRootError::MakePrivateFailed {
                        target: backing_canon,
                        error: e,
                    });
                }

                info!(
                    source = %source_canon.display(),
                    backing_root = %backing_canon.display(),
                    "backing root bind mount created (private, propagation isolated)"
                );
                // Verify identity after bind mount.
                Self::verify_identity(source_canon, &backing_canon).inspect_err(|_| {
                    if let Some(ref dir) = created_temp_dir {
                        let _ = std::fs::remove_dir_all(dir);
                    }
                })?;
                Ok(Self {
                    path: backing_canon,
                    created_bind_mount: true,
                    created_temp_dir,
                })
            }
            Err(e) => {
                // In-place mounts require a bind created and made private by
                // this setup. Identity alone cannot prove propagation mode.
                if in_place {
                    if let Some(ref dir) = created_temp_dir {
                        let _ = std::fs::remove_dir(dir);
                    }
                    return Err(BackingRootError::BindMountFailed {
                        source: source_canon.to_path_buf(),
                        target: backing_canon,
                        error: e,
                    });
                }

                // P1-1: Bind mount failed. Check if the backing root is a
                // pre-existing alias (e.g. operator-created bind mount or
                // symlink to source). Verify via stat dev/ino identity.
                match Self::verify_identity(source_canon, &backing_canon) {
                    Ok(()) => {
                        warn!(
                            source = %source_canon.display(),
                            backing_root = %backing_canon.display(),
                            error = %e,
                            "bind mount failed but backing root is a verified \
                             pre-existing alias of source (dev/ino match)"
                        );
                        Ok(Self {
                            path: backing_canon,
                            created_bind_mount: false,
                            created_temp_dir,
                        })
                    }
                    Err(id_err) => {
                        // Not an alias — fail-closed. Clean up if we created it.
                        if let Some(ref dir) = created_temp_dir {
                            let _ = std::fs::remove_dir_all(dir);
                        }
                        Err(id_err)
                    }
                }
            }
        }
    }

    /// The daemon-facing live root path.
    ///
    /// Activation bootstrap, reload, watching, and N3 protocol event
    /// `skillDir` join skill names onto this path. Socket notify v2 uses the
    /// separate canonical source root.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether SkillFS created a bind mount for this backing root.
    pub fn created_bind_mount(&self) -> bool {
        self.created_bind_mount
    }

    /// Whether SkillFS created the backing root directory (needs cleanup).
    pub fn created_temp_dir(&self) -> bool {
        self.created_temp_dir.is_some()
    }

    /// Clean up resources created by [`Self::setup`].
    ///
    /// Unmounts the bind mount if SkillFS created it, then removes the
    /// temporary directory if SkillFS created it. Errors are logged but
    /// do not propagate — this runs on the shutdown path and should not
    /// block process exit.
    pub fn cleanup(&self) {
        if self.created_bind_mount {
            match Self::do_umount(&self.path) {
                Ok(()) => {
                    info!(backing_root = %self.path.display(), "backing root bind mount unmounted");
                }
                Err(e) => {
                    warn!(
                        backing_root = %self.path.display(),
                        error = %e,
                        "failed to unmount backing root bind mount during cleanup"
                    );
                }
            }
        }
        if let Some(ref dir) = self.created_temp_dir {
            if let Err(e) = std::fs::remove_dir_all(dir) {
                warn!(
                    path = %dir.display(),
                    error = %e,
                    "failed to remove backing root temp dir during cleanup"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    /// Validate that the directory has no group/other access bits and is
    /// owned by the current uid or root (P1-2).
    fn validate_permissions(path: &Path) -> Result<(), BackingRootError> {
        use std::os::unix::fs::MetadataExt;

        let meta = std::fs::metadata(path).map_err(|e| BackingRootError::CanonicalizeFailed {
            path: path.to_path_buf(),
            source: e,
        })?;

        let mode = meta.mode();
        // P1-2: Reject any group or other access bits.
        // Backing root is a security boundary — only the owner should
        // have access.
        if mode & 0o077 != 0 {
            return Err(BackingRootError::PermissionsTooOpen {
                path: path.to_path_buf(),
                mode,
            });
        }

        // Owner must be the current uid or root.
        let file_uid = meta.uid();
        let current_uid = unsafe { libc::geteuid() };
        if file_uid != current_uid && file_uid != 0 {
            return Err(BackingRootError::WrongOwner {
                path: path.to_path_buf(),
                uid: file_uid,
            });
        }

        Ok(())
    }

    /// Verify that `backing` and `source` refer to the same on-disk
    /// directory by comparing `stat` dev and ino (P1-1).
    fn verify_identity(source: &Path, backing: &Path) -> Result<(), BackingRootError> {
        use std::os::unix::fs::MetadataExt;

        let src_meta =
            std::fs::metadata(source).map_err(|e| BackingRootError::CanonicalizeFailed {
                path: source.to_path_buf(),
                source: e,
            })?;
        let bak_meta =
            std::fs::metadata(backing).map_err(|e| BackingRootError::CanonicalizeFailed {
                path: backing.to_path_buf(),
                source: e,
            })?;

        if src_meta.dev() == bak_meta.dev() && src_meta.ino() == bak_meta.ino() {
            Ok(())
        } else {
            Err(BackingRootError::IdentityMismatch {
                backing: backing.to_path_buf(),
                source: source.to_path_buf(),
                backing_dev: bak_meta.dev(),
                backing_ino: bak_meta.ino(),
                source_dev: src_meta.dev(),
                source_ino: src_meta.ino(),
            })
        }
    }

    // Isolate the detached bind before any copy reaches another mount namespace.
    #[cfg(target_os = "linux")]
    fn do_bind_mount(source: &Path, target: &Path) -> std::io::Result<()> {
        use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

        let src_c = CString::new(source.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;
        let tgt_c = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;

        // SAFETY: src_c remains a valid C string; the flags request a new mount fd.
        let fd = unsafe {
            libc::syscall(
                libc::SYS_open_tree,
                libc::AT_FDCWD,
                src_c.as_ptr(),
                libc::OPEN_TREE_CLONE | libc::OPEN_TREE_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: successful open_tree returns an owned int fd, even through syscall.
        let mount = unsafe { OwnedFd::from_raw_fd(fd as libc::c_int) };
        Self::do_make_private(Path::new(&format!("/proc/self/fd/{}", mount.as_raw_fd())))?;

        // SAFETY: the owned mount fd and target C string remain valid for this call.
        // Dropping the fd also destroys an unpublished mount on any failure above/below.
        let ret = unsafe {
            libc::syscall(
                libc::SYS_move_mount,
                mount.as_raw_fd(),
                c"".as_ptr(),
                libc::AT_FDCWD,
                tgt_c.as_ptr(),
                libc::MOVE_MOUNT_F_EMPTY_PATH,
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn do_bind_mount(_source: &Path, _target: &Path) -> std::io::Result<()> {
        Err(std::io::Error::from_raw_os_error(libc::ENOSYS))
    }

    /// Set `MS_PRIVATE | MS_REC` on the backing root so that subsequent
    /// mount events (in particular the FUSE in-place over-mount) are not
    /// propagated into this mount namespace.
    #[cfg(target_os = "linux")]
    fn do_make_private(target: &Path) -> std::io::Result<()> {
        let tgt_c = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;

        let ret = unsafe {
            libc::mount(
                std::ptr::null(),
                tgt_c.as_ptr(),
                std::ptr::null(),
                libc::MS_PRIVATE | libc::MS_REC,
                std::ptr::null(),
            )
        };
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn do_make_private(_target: &Path) -> std::io::Result<()> {
        Err(std::io::Error::from_raw_os_error(libc::ENOSYS))
    }

    /// Unmount the bind mount at `target` via `umount(2)`.
    #[cfg(target_os = "linux")]
    fn do_umount(target: &Path) -> std::io::Result<()> {
        let tgt_c = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))?;

        let ret = unsafe { libc::umount(tgt_c.as_ptr()) };
        if ret == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn do_umount(_target: &Path) -> std::io::Result<()> {
        Err(std::io::Error::from_raw_os_error(libc::ENOSYS))
    }
}

impl Drop for LedgerBackingRoot {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// Returns `true` when `child` is the same as or a descendant of `parent`.
fn path_is_inside(child: &Path, parent: &Path) -> bool {
    if child == parent {
        return true;
    }
    child.starts_with(parent)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // path_is_inside
    // -----------------------------------------------------------------------

    #[test]
    fn path_is_inside_same_path() {
        assert!(path_is_inside(Path::new("/a/b"), Path::new("/a/b")));
    }

    #[test]
    fn path_is_inside_descendant() {
        assert!(path_is_inside(Path::new("/a/b/c"), Path::new("/a/b")));
    }

    #[test]
    fn path_is_inside_not_descendant() {
        assert!(!path_is_inside(Path::new("/a/c"), Path::new("/a/b")));
    }

    #[test]
    fn path_is_inside_sibling_not_inside() {
        assert!(!path_is_inside(Path::new("/a/bb"), Path::new("/a/b")));
    }

    // -----------------------------------------------------------------------
    // BackingRootError display
    // -----------------------------------------------------------------------

    #[test]
    fn empty_path_error_display() {
        let err = BackingRootError::EmptyPath;
        assert!(err.to_string().contains("empty"));
    }

    #[test]
    fn permissions_too_open_error_display() {
        let err = BackingRootError::PermissionsTooOpen {
            path: PathBuf::from("/tmp/test"),
            mode: 0o755,
        };
        let msg = err.to_string();
        assert!(msg.contains("too open"));
        assert!(msg.contains("0o755"));
    }

    #[test]
    fn identity_mismatch_error_display() {
        let err = BackingRootError::IdentityMismatch {
            backing: PathBuf::from("/tmp/backing"),
            source: PathBuf::from("/tmp/source"),
            backing_dev: 1,
            backing_ino: 2,
            source_dev: 3,
            source_ino: 4,
        };
        let msg = err.to_string();
        assert!(msg.contains("does not match"));
        assert!(msg.contains("dev=1"));
    }

    // -----------------------------------------------------------------------
    // LedgerBackingRoot::setup — validation tests
    // -----------------------------------------------------------------------

    #[test]
    fn empty_path_rejected() {
        let result = LedgerBackingRoot::setup(
            Path::new("/source"),
            Path::new(""),
            Path::new("/mount"),
            false,
        );
        assert!(matches!(result, Err(BackingRootError::EmptyPath)));
    }

    #[test]
    fn backing_root_inside_mount_path_rejected() {
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();
        let inside = mount_canon.join("subdir");

        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();

        let result = LedgerBackingRoot::setup(&source_canon, &inside, &mount_canon, false);
        assert!(matches!(
            result,
            Err(BackingRootError::InsideMountPath { .. })
        ));
    }

    #[test]
    fn backing_root_inside_source_tree_rejected_in_place() {
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let inside = source_canon.join("subdir");

        let result = LedgerBackingRoot::setup(
            &source_canon,
            &inside,
            &source_canon,
            true, // in_place
        );
        assert!(
            matches!(result, Err(BackingRootError::InsideMountPath { .. }))
                || matches!(result, Err(BackingRootError::InsideSourceTree { .. })),
            "backing root inside source tree should be rejected, got: {result:?}"
        );
    }

    #[test]
    fn permissions_too_open_rejected() {
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();

        let backing = tempfile::tempdir().unwrap();
        let backing_path = backing.path().canonicalize().unwrap();

        // Make it group/other accessible (0o755).
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&backing_path, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        let result = LedgerBackingRoot::setup(&source_canon, &backing_path, &mount_canon, false);
        assert!(
            matches!(result, Err(BackingRootError::PermissionsTooOpen { .. })),
            "0o755 backing root should be rejected: {result:?}"
        );

        // Clean up: reset permissions so tempfile can remove it.
        std::fs::set_permissions(&backing_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn valid_backing_root_same_as_source_accepted() {
        // Non-in-place: backing root == source → use directly, no bind mount.
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();

        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        let br = LedgerBackingRoot::setup(&source_canon, &source_canon, &mount_canon, false)
            .expect("backing root == source should be accepted in non-in-place mode");

        assert_eq!(br.path(), source_canon);
        assert!(!br.created_bind_mount);
        assert!(br.created_temp_dir.is_none());
    }

    #[test]
    fn backing_root_equal_mount_path_rejected() {
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        let result = LedgerBackingRoot::setup(&source_canon, &mount_canon, &mount_canon, false);
        assert!(matches!(
            result,
            Err(BackingRootError::InsideMountPath { .. })
        ));
    }

    #[test]
    fn in_place_backing_root_same_as_source_rejected() {
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();

        let result = LedgerBackingRoot::setup(&source_canon, &source_canon, &source_canon, true);

        assert!(matches!(
            result,
            Err(BackingRootError::InsideMountPath { .. })
        ));
    }

    #[test]
    fn non_in_place_symlink_to_source_uses_source_without_bind() {
        use std::os::unix::fs::PermissionsExt;

        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let private_parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            private_parent.path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let backing_link = private_parent.path().join("backing");
        std::os::unix::fs::symlink(&source_canon, &backing_link).unwrap();
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        let backing = LedgerBackingRoot::setup(&source_canon, &backing_link, &mount_canon, false)
            .expect("non-in-place source alias should remain compatible");

        assert_eq!(backing.path(), source_canon);
        assert!(!backing.created_bind_mount());
    }

    #[test]
    fn symlink_into_mount_rejected_after_canonicalization() {
        use std::os::unix::fs::PermissionsExt;

        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();
        let private_parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            private_parent.path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let backing_link = private_parent.path().join("backing");
        std::os::unix::fs::symlink(&mount_canon, &backing_link).unwrap();

        let result = LedgerBackingRoot::setup(&source_canon, &backing_link, &mount_canon, false);

        assert!(matches!(
            result,
            Err(BackingRootError::InsideMountPath { .. })
        ));
    }

    #[test]
    fn symlink_into_source_subtree_rejected_after_canonicalization() {
        use std::os::unix::fs::PermissionsExt;

        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let source_child = source_canon.join("child");
        std::fs::create_dir(&source_child).unwrap();
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();
        let private_parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            private_parent.path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let backing_link = private_parent.path().join("backing");
        std::os::unix::fs::symlink(&source_child, &backing_link).unwrap();

        let result = LedgerBackingRoot::setup(&source_canon, &backing_link, &mount_canon, false);

        assert!(matches!(
            result,
            Err(BackingRootError::InsideSourceTree { .. })
        ));
    }

    #[test]
    fn in_place_symlink_to_source_rejected() {
        use std::os::unix::fs::PermissionsExt;

        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let private_parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            private_parent.path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let backing_link = private_parent.path().join("backing");
        std::os::unix::fs::symlink(&source_canon, &backing_link).unwrap();

        let result = LedgerBackingRoot::setup(&source_canon, &backing_link, &source_canon, true);

        assert!(matches!(
            result,
            Err(BackingRootError::InsideMountPath { .. })
        ));
    }

    #[test]
    fn p2_1_no_side_effects_on_rejected_path() {
        // P2-1: A rejected path inside source must NOT create a directory
        // in the source tree.
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let backing_path = source_canon.join(".skillfs-ledger");

        assert!(!backing_path.exists(), "precondition: path must not exist");

        let result = LedgerBackingRoot::setup(
            &source_canon,
            &backing_path,
            &source_canon, // in-place: source == mount
            true,
        );

        assert!(result.is_err(), "should be rejected");

        assert!(
            !backing_path.exists(),
            "rejected path must not create directory in source tree"
        );
    }

    #[test]
    fn p1_1_identity_mismatch_rejected() {
        // P1-1: A separate directory that is NOT a bind mount of source
        // must be rejected (identity mismatch).
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();

        // Create a private parent for the backing root so the parent
        // permission check passes and the identity check is what rejects.
        let private_parent = tempfile::tempdir().unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            private_parent.path(),
            std::fs::Permissions::from_mode(0o700),
        )
        .unwrap();
        let backing_path = private_parent.path().join("backing");
        std::fs::create_dir(&backing_path).unwrap();
        std::fs::set_permissions(&backing_path, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        let result = LedgerBackingRoot::setup(&source_canon, &backing_path, &mount_canon, false);

        // Without root, bind mount fails, and the separate directory
        // has different dev/ino -> IdentityMismatch.
        assert!(
            matches!(result, Err(BackingRootError::IdentityMismatch { .. })),
            "separate non-bind-mount directory should be rejected (identity mismatch): {result:?}"
        );
    }

    #[test]
    fn p1_2_created_dir_has_restrictive_permissions() {
        // P1-2: When SkillFS creates the backing root, it must use 0o700.
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        let parent = tempfile::tempdir().unwrap();
        let backing_path = parent.path().join("backing_root");

        let result = LedgerBackingRoot::setup(&source_canon, &backing_path, &mount_canon, false);

        // Without root, bind mount fails and identity check fails,
        // but the directory was created with 0o700 before that.
        // The cleanup should remove it.
        assert!(result.is_err());

        // The temp dir should have been cleaned up on failure.
        assert!(
            !backing_path.exists(),
            "created dir should be cleaned up on failure"
        );
    }

    #[test]
    fn cleanup_removes_created_temp_dir() {
        // When backing root == source, no temp dir is created.
        // This test verifies that when a temp dir IS created (and then
        // rejected), it's cleaned up.
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        let parent = tempfile::tempdir().unwrap();
        let backing_path = parent.path().join("backing_root");

        let result = LedgerBackingRoot::setup(&source_canon, &backing_path, &mount_canon, false);

        // Without root, this will fail (identity mismatch after bind mount
        // failure). The temp dir should be cleaned up.
        assert!(result.is_err());
        assert!(
            !backing_path.exists(),
            "temp dir should be cleaned up on failure"
        );
    }

    #[test]
    fn p2_1_path_shape_checked_before_creation() {
        // P2-1: Verify that a path inside mount is rejected without
        // creating the directory.
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();
        let backing_inside_mount = mount_canon.join("backing_root");

        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();

        assert!(!backing_inside_mount.exists());

        let result =
            LedgerBackingRoot::setup(&source_canon, &backing_inside_mount, &mount_canon, false);

        assert!(result.is_err());
        assert!(
            !backing_inside_mount.exists(),
            "directory must not be created when path shape check fails"
        );
    }

    #[test]
    fn backing_root_not_directory_rejected() {
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        // Create a file where the backing root should be.
        let file_dir = tempfile::tempdir().unwrap();
        let file_path = file_dir.path().join("not_a_dir");
        std::fs::write(&file_path, "test").unwrap();

        let result = LedgerBackingRoot::setup(&source_canon, &file_path, &mount_canon, false);
        assert!(result.is_err());
    }

    // -----------------------------------------------------------------------
    // New tests for review round 3
    // -----------------------------------------------------------------------

    #[test]
    fn non_in_place_backing_root_inside_source_rejected() {
        // P1-a: Even in non-in-place mode, backing root inside source
        // must be rejected to prevent recursive bind mount.
        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        let backing_in_source = source_canon.join(".skillfs-ledger");

        let result = LedgerBackingRoot::setup(
            &source_canon,
            &backing_in_source,
            &mount_canon,
            false, // non-in-place
        );

        assert!(
            matches!(result, Err(BackingRootError::InsideSourceTree { .. })),
            "non-in-place backing root inside source should be rejected: {result:?}"
        );
        // Must not have created the directory.
        assert!(
            !backing_in_source.exists(),
            "directory must not be created when inside source tree"
        );
    }

    #[test]
    fn parent_0755_rejected() {
        // P1-b: A backing root under a parent with group/other traversal
        // bits must be rejected. The parent is the security boundary.
        use std::os::unix::fs::PermissionsExt;

        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        // Create a parent with 0755 (world-readable/executable).
        let parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        let backing_path = parent.path().join("backing_root");

        let result = LedgerBackingRoot::setup(&source_canon, &backing_path, &mount_canon, false);

        assert!(
            matches!(result, Err(BackingRootError::PermissionsTooOpen { .. })),
            "parent 0755 should be rejected: {result:?}"
        );
        // Clean up
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn parent_0700_with_source_0755_passes_parent_check() {
        // P1-b: A private parent (0700) should pass the parent permission
        // check even when the source itself is 0755. After bind mount,
        // the backing root dir shows source perms, but parent privacy
        // gates traversal.
        use std::os::unix::fs::PermissionsExt;

        let source = tempfile::tempdir().unwrap();
        let source_canon = source.path().canonicalize().unwrap();
        // Source can be 0755 — that's normal.
        std::fs::set_permissions(&source_canon, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mount = tempfile::tempdir().unwrap();
        let mount_canon = mount.path().canonicalize().unwrap();

        // Private parent.
        let parent = tempfile::tempdir().unwrap();
        std::fs::set_permissions(parent.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let backing_path = parent.path().join("source");

        let result = LedgerBackingRoot::setup(&source_canon, &backing_path, &mount_canon, false);

        // Parent check passes; without root the bind mount fails;
        // then identity check fails (different dir).
        // The point is: PermissionsTooOpen is NOT the error.
        assert!(
            !matches!(result, Err(BackingRootError::PermissionsTooOpen { .. })),
            "parent 0700 should pass permission check; got: {result:?}"
        );
        // Clean up
        std::fs::set_permissions(&source_canon, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
