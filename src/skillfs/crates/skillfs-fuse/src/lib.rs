//! FUSE virtual filesystem layer for SkillFS.
//!
//! Exposes skills as a virtual filesystem. The default view (from
//! \`skillfs-views.toml\`) is shown directly under \`/skills/\`. Secondary
//! views are accessible via the always-visible \`skill-discover\` virtual
//! skill, which lists readable paths for opening them directly. Paths point
//! to the physical source by default and can be mapped into a shared FUSE view
//! for readers in another mount namespace.
#![allow(clippy::too_many_arguments)]

use std::path::PathBuf;

use thiserror::Error;
use tracing::info;

pub mod security;
pub mod symlink_policy;

mod attr;
mod fs;
mod handles;
mod inode;
mod mount;
pub mod path;
pub mod proc_mounts;
mod sync;
mod sys;
mod xattr;

pub use fs::SkillFs;
pub use mount::{MountConfig, mount_background_configured, mount_configured};
pub use path::{SkillLayout, detect_skill_layout};

#[allow(deprecated)]
pub use mount::{
    mount, mount_background, mount_background_with_security,
    mount_background_with_security_active_resolver_and_demo_refresh,
    mount_background_with_security_active_resolver_demo_refresh_and_trusted_writer,
    mount_background_with_security_and_active_resolver, mount_with_security,
    mount_with_security_active_resolver_and_demo_refresh,
    mount_with_security_active_resolver_demo_refresh_and_trusted_writer,
    mount_with_security_and_active_resolver,
};

// ---------------------------------------------------------------------------
// Error Types
// ---------------------------------------------------------------------------

#[derive(Debug, Error)]
pub enum FuseError {
    #[error("mount failed: {0}")]
    MountFailed(String),
    #[error("unmount failed: {0}")]
    UnmountFailed(String),
    #[error("invalid mount point: {0}")]
    InvalidMountPoint(String),
    #[error("invalid skill-discover root: {0}")]
    InvalidSkillDiscoverRoot(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("io error: {0}")]
    IoError(#[from] std::io::Error),
}

// ---------------------------------------------------------------------------
// Mount Options
// ---------------------------------------------------------------------------

/// Mount options for the FUSE filesystem.
#[derive(Debug, Clone)]
pub struct MountOptions {
    /// Allow other users to access the mount (requires allow_other in fuse.conf)
    pub allow_other: bool,
    /// Reject mutations through the FUSE mount at the kernel boundary.
    pub read_only: bool,
    /// Run in foreground (don't daemonize)
    pub foreground: bool,
    /// Additional FUSE mount options
    pub fuse_options: Vec<String>,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            allow_other: false,
            read_only: false,
            foreground: false,
            fuse_options: vec!["noatime".to_string()],
        }
    }
}

// ---------------------------------------------------------------------------
// Mount Handle
// ---------------------------------------------------------------------------

/// Handle to a mounted FUSE filesystem.
pub struct MountHandle {
    /// The mount point path
    pub mountpoint: PathBuf,
    /// Background session (if mounted in background).
    /// Wrapped in `Option` so both explicit `unmount` and `Drop` can
    /// take it without double-joining.
    session: Option<std::thread::JoinHandle<()>>,
}

impl MountHandle {
    /// Unmount the filesystem and wait for the FUSE event loop thread
    /// to exit.
    pub fn unmount(mut self) -> Result<(), FuseError> {
        self.unmount_inner()
    }

    fn unmount_inner(&mut self) -> Result<(), FuseError> {
        info!(mountpoint = %self.mountpoint.display(), "unmounting filesystem");

        #[cfg(target_os = "linux")]
        {
            // Raw OS-string argument (see `unmount_commands`): a lossy UTF-8
            // view of a mountpoint with invalid bytes names a different
            // (nonexistent) path, so fusermount3 would fail and teardown
            // would fall into the force loop below, which retries the wrong
            // path 50 times and still leaks the mount.
            let (program, argv) = &Self::unmount_commands(&self.mountpoint)[0];
            let output = std::process::Command::new(program).args(argv).output();

            match output {
                Ok(output) if output.status.success() => {
                    info!("unmount successful");
                }
                Ok(output) => {
                    // A plain `fusermount3 -u` can fail transiently (busy /
                    // lazy). Best-effort force cleanup so we never leave the
                    // mountpoint dangling, but still surface the original
                    // failure to the caller for explicit `unmount()`.
                    //
                    // Detach the session thread first so a later `Drop` sees
                    // `session == None` and skips a second `unmount_inner`
                    // (which would run `force_unmount_path` again — up to a
                    // full 5s each on a genuinely stuck mount).
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    self.session.take();
                    Self::force_unmount_path(&self.mountpoint);
                    return Err(FuseError::UnmountFailed(stderr.to_string()));
                }
                Err(e) => {
                    self.session.take();
                    Self::force_unmount_path(&self.mountpoint);
                    return Err(FuseError::IoError(e));
                }
            }
        }

        if let Some(handle) = self.session.take() {
            if handle.join().is_err() {
                return Err(FuseError::UnmountFailed(
                    "FUSE event loop thread panicked".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Check if the mount is still active.
    pub fn is_mounted(&self) -> bool {
        std::fs::metadata(&self.mountpoint).is_ok()
    }

    /// Return `true` when `path` currently appears as a mountpoint in
    /// `/proc/mounts`. This is the authoritative signal (unlike
    /// `std::fs::metadata`, which can succeed on a dead FUSE endpoint).
    #[cfg(target_os = "linux")]
    fn path_is_mounted(path: &std::path::Path) -> bool {
        // Raw bytes on both sides: a lossy UTF-8 view would conflate a
        // mounted invalid-byte path with a queried U+FFFD path, and
        // read_to_string fails wholesale on any non-UTF-8 mount line.
        use std::os::unix::ffi::OsStrExt;
        let Ok(mounts) = std::fs::read("/proc/mounts") else {
            return false;
        };
        crate::proc_mounts::mounts_contain_target(&mounts, path.as_os_str().as_bytes())
    }

    /// The unmount invocations for `mountpoint`, in escalation order: plain
    /// `fusermount3 -u`, lazy `fusermount3 -u -z`, then `umount -l`. Element
    /// 0 is the argv `unmount_inner` uses for its primary unmount attempt.
    ///
    /// The mountpoint rides as the raw OS string in every argv. The mount
    /// *detection* side is byte-exact (`proc_mounts`), and the FUSE session
    /// itself mounts such paths as raw bytes; a `to_string_lossy()` view
    /// would replace an invalid byte with U+FFFD, so every command would
    /// target a nonexistent path and the bounded force-unmount loop would
    /// retry the wrong path until the mount leaks.
    #[cfg(target_os = "linux")]
    fn unmount_commands(
        mountpoint: &std::path::Path,
    ) -> [(&'static str, Vec<std::ffi::OsString>); 3] {
        let raw = mountpoint.as_os_str().to_os_string();
        [
            ("fusermount3", vec!["-u".into(), raw.clone()]),
            ("fusermount3", vec!["-u".into(), "-z".into(), raw.clone()]),
            ("umount", vec!["-l".into(), raw]),
        ]
    }

    /// One best-effort unmount pass: plain `fusermount3 -u`, then lazy
    /// `fusermount3 -u -z`, then `umount -l`. All failures are ignored; the
    /// caller re-checks `/proc/mounts` to decide whether to retry.
    #[cfg(target_os = "linux")]
    fn try_unmount_once(path: &std::path::Path) {
        for (program, args) in Self::unmount_commands(path) {
            let _ = std::process::Command::new(program).args(&args).output();
        }
    }

    /// Bounded, non-panicking force cleanup of a mountpoint. Returns as soon
    /// as the path is no longer mounted; otherwise retries a fixed number of
    /// times before giving up with a warning. Used by both `Drop` and the
    /// explicit `unmount()` error path so every test/handle teardown route is
    /// covered — a leaked FUSE mount under a workspace directory is far worse
    /// than a slow teardown.
    #[cfg(target_os = "linux")]
    fn force_unmount_path(path: &std::path::Path) {
        for _ in 0..50 {
            if !Self::path_is_mounted(path) {
                return;
            }
            Self::try_unmount_once(path);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }

        if Self::path_is_mounted(path) {
            eprintln!(
                "WARN: leaked SkillFS FUSE mount at {} (force cleanup exhausted)",
                path.display()
            );
        }
    }
}

impl Drop for MountHandle {
    fn drop(&mut self) {
        // Best-effort teardown. Must never panic (a panic while unwinding a
        // failing test would abort the process) and must never block
        // unboundedly.
        //
        // On a clean unmount `unmount_inner` already joins the session thread.
        // We deliberately do NOT force a join afterwards: if the mountpoint
        // could only be torn down lazily, the FUSE session thread may never
        // return, and joining it would hang. Dropping the `JoinHandle` simply
        // detaches the thread, which is safe once the mount is gone.
        if self.session.is_some() {
            let _ = self.unmount_inner();
        }

        // Cover every path — including tests that just `drop(handle)` — by
        // force-cleaning the mountpoint even when `unmount_inner` bailed out
        // early on a `fusermount3` error. Bounded (see `force_unmount_path`).
        #[cfg(target_os = "linux")]
        Self::force_unmount_path(&self.mountpoint);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inode::InodeManager;
    use crate::path::{PathType, parse_path};
    use fuser::{FUSE_ROOT_ID, FileType};
    use std::path::Path;

    #[test]
    fn test_parse_path_root() {
        assert_eq!(parse_path(Path::new("/"), false), PathType::Root);
    }

    /// Every unmount invocation must address the mountpoint as raw OS-string
    /// bytes. The CLI accepts non-UTF-8 mountpoints (`args_os`), the FUSE
    /// session mounts them as raw bytes, and the mount detection
    /// (`proc_mounts`) matches byte-exactly — a `to_string_lossy()` argv
    /// would name a different (U+FFFD-mangled) path, so every unmount
    /// attempt would fail and the bounded force loop would leak the mount.
    #[cfg(target_os = "linux")]
    #[test]
    fn unmount_commands_pass_the_mountpoint_as_raw_bytes() {
        use std::ffi::{OsStr, OsString};
        use std::os::unix::ffi::OsStrExt;

        let mountpoint = Path::new(OsStr::from_bytes(b"/srv/mount-\xff"));
        let commands = MountHandle::unmount_commands(mountpoint);

        assert_eq!(commands[0].0, "fusermount3");
        assert_eq!(commands[1].0, "fusermount3");
        assert_eq!(commands[2].0, "umount");

        for (index, (_, argv)) in commands.iter().enumerate() {
            let mounted = argv
                .iter()
                .find(|arg| arg.as_encoded_bytes().contains(&0xff))
                .unwrap_or_else(|| panic!("argv {index} must carry the raw mountpoint: {argv:?}"));
            assert_eq!(
                mounted.as_os_str(),
                OsStr::from_bytes(b"/srv/mount-\xff"),
                "argv {index} must address the mountpoint as raw bytes, not a \
                 lossy U+FFFD view"
            );
        }

        // Option spelling sanity for the escalation order.
        let raw = OsStr::from_bytes(b"/srv/mount-\xff").to_os_string();
        let expected: Vec<Vec<OsString>> = vec![
            vec!["-u".into(), raw.clone()],
            vec!["-u".into(), "-z".into(), raw.clone()],
            vec!["-l".into(), raw],
        ];
        for (index, ((_, argv), want)) in commands.iter().zip(&expected).enumerate() {
            assert_eq!(argv, want, "argv {index} spelling");
        }
    }

    #[test]
    fn configured_mount_rejects_relative_skill_discover_root() {
        let source = tempfile::tempdir().expect("source tempdir");
        let mountpoint = tempfile::tempdir().expect("mountpoint tempdir");
        let store = std::sync::Arc::new(parking_lot::RwLock::new(
            skillfs_core::store::SkillStore::new(),
        ));

        let error = mount_configured(
            mountpoint.path(),
            source.path(),
            store,
            MountOptions::default(),
            false,
            MountConfig {
                skill_discover_root: Some("relative/skills".into()),
                ..MountConfig::default()
            },
        )
        .expect_err("relative discover root must fail before FUSE");

        assert!(matches!(error, FuseError::InvalidSkillDiscoverRoot(_)));
    }

    #[test]
    fn test_parse_path_skills_dir() {
        assert_eq!(parse_path(Path::new("/skills"), false), PathType::SkillsDir);
    }

    #[test]
    fn test_parse_path_skill_dir() {
        assert_eq!(
            parse_path(Path::new("/skills/web-search"), false),
            PathType::SkillDir {
                skill_name: "web-search".to_string()
            }
        );
    }

    #[test]
    fn test_parse_path_skill_md() {
        assert_eq!(
            parse_path(Path::new("/skills/web-search/SKILL.md"), false),
            PathType::SkillMd {
                skill_name: "web-search".to_string()
            }
        );
    }

    #[test]
    fn test_parse_path_passthrough() {
        assert_eq!(
            parse_path(Path::new("/skills/web-search/scripts/run.sh"), false),
            PathType::Passthrough {
                skill_name: "web-search".to_string(),
                relative_path: PathBuf::from("scripts/run.sh"),
            }
        );
    }

    #[test]
    fn test_parse_path_invalid() {
        assert_eq!(
            parse_path(Path::new("/unknown-file"), false),
            PathType::Invalid
        );
    }

    #[test]
    fn test_mount_options_default() {
        let opts = MountOptions::default();
        assert!(!opts.allow_other);
        assert!(!opts.foreground);
        assert!(opts.fuse_options.contains(&"noatime".to_string()));
    }

    #[test]
    fn test_inode_manager_allocate() {
        let manager = InodeManager::new();
        assert!(manager.get(FUSE_ROOT_ID).is_some());
        assert_eq!(manager.get_path(FUSE_ROOT_ID), Some("/".to_string()));

        let ino = manager.allocate("/test", FileType::RegularFile, FUSE_ROOT_ID);
        assert!(ino > FUSE_ROOT_ID);
        assert_eq!(manager.get_path(ino), Some("/test".to_string()));

        let ino2 = manager.allocate("/test", FileType::RegularFile, FUSE_ROOT_ID);
        assert_eq!(ino, ino2);
    }

    #[test]
    fn test_inode_manager_lookup_by_path() {
        let manager = InodeManager::new();
        assert_eq!(manager.lookup_by_path("/"), Some(FUSE_ROOT_ID));
        assert_eq!(manager.lookup_by_path("/unknown"), None);

        let ino = manager.allocate("/new_file", FileType::RegularFile, FUSE_ROOT_ID);
        assert_eq!(manager.lookup_by_path("/new_file"), Some(ino));
    }

    #[test]
    fn test_parse_path_edge_cases() {
        assert_eq!(
            parse_path(Path::new("/unknown-file"), false),
            PathType::Invalid
        );
        assert_eq!(
            parse_path(Path::new("/skills/web-search/a/b/c/d.txt"), false),
            PathType::Passthrough {
                skill_name: "web-search".to_string(),
                relative_path: PathBuf::from("a/b/c/d.txt"),
            }
        );
    }

    #[test]
    fn test_parse_path_in_place() {
        assert_eq!(parse_path(Path::new("/"), true), PathType::SkillsDir);
        assert_eq!(
            parse_path(Path::new("/github"), true),
            PathType::SkillDir {
                skill_name: "github".to_string()
            }
        );
        assert_eq!(
            parse_path(Path::new("/github/SKILL.md"), true),
            PathType::SkillMd {
                skill_name: "github".to_string()
            }
        );
        assert_eq!(
            parse_path(Path::new("/github/scripts/run.sh"), true),
            PathType::Passthrough {
                skill_name: "github".to_string(),
                relative_path: PathBuf::from("scripts/run.sh"),
            }
        );
    }

    #[test]
    fn unmount_inner_detaches_session_when_fusermount_fails() {
        let mut handle = MountHandle {
            mountpoint: PathBuf::from("/nonexistent/mount/point"),
            session: Some(std::thread::spawn(|| {})),
        };
        let result = handle.unmount_inner();
        assert!(result.is_err(), "bogus mountpoint must produce an error");
        // The error path detaches the session (takes it without joining) so a
        // subsequent `Drop` sees `session == None` and does not run a second
        // `unmount_inner` / `force_unmount_path` pass.
        assert!(
            handle.session.is_none(),
            "session must be detached when fusermount3 fails so Drop skips re-unmount"
        );
    }

    #[test]
    fn drop_with_failed_fusermount_does_not_block() {
        use std::time::{Duration, Instant};

        let handle = MountHandle {
            mountpoint: PathBuf::from("/nonexistent/mount/point"),
            session: Some(std::thread::spawn(|| {})),
        };
        let start = Instant::now();
        drop(handle);
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "Drop must not block on failed fusermount3"
        );
    }

    #[test]
    fn drop_without_session_is_noop() {
        let handle = MountHandle {
            mountpoint: PathBuf::from("/tmp/test-mount"),
            session: None,
        };
        drop(handle);
    }
}
