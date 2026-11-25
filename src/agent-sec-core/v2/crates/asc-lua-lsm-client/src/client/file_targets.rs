//! Resolve policy roots into the paths exposed by Linux file and dentry hooks.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use asc_file_policy::FileProtectionTargetKind;
use asc_lua_lsm_protocol::{LuaLsmPlan, PathView};
use asc_policy_types::target::Failure;
use rustix::fs::{AtFlags, FileType, Mode, OFlags, ResolveFlags, StatxFlags};

use super::{local_error, rejected, retryable, verify_process};

// A disappearance after a successful lookup is a namespace change, independent of securityfs.
pub(super) fn target_error(error: io::Error) -> Failure {
    match error.kind() {
        io::ErrorKind::NotFound => retryable("LUA_LSM_TARGET_CHANGED"),
        io::ErrorKind::NotADirectory => rejected("LUA_LSM_TARGET_TYPE_MISMATCH"),
        _ => local_error(error),
    }
}

pub(super) fn resolve_target(path: &Path) -> Result<Option<PathBuf>, Failure> {
    match fs::canonicalize(path) {
        Ok(path) => Ok(Some(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_symlink() => {
                Err(rejected("LUA_LSM_DANGLING_TARGET_SYMLINK"))
            }
            Ok(_) => Err(retryable("LUA_LSM_TARGET_CHANGED")),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(target_error(error)),
        },
        Err(error) => Err(target_error(error)),
    }
}

struct Mount {
    id: u64,
    device: Vec<u8>,
    root: PathBuf,
    point: PathBuf,
    filesystem: String,
}

impl Mount {
    fn check_mapping_identity(&self) -> Result<(), Failure> {
        match self.filesystem.as_str() {
            "overlay" | "overlayfs" => Err(rejected("LUA_LSM_OVERLAYFS_UNSUPPORTED")),
            "btrfs" => Err(rejected("LUA_LSM_BTRFS_UNSUPPORTED")),
            _ => Ok(()),
        }
    }
}

struct MountView {
    root: OwnedFd,
    mounts: Vec<Mount>,
}

impl MountView {
    fn read(proc: &Path) -> Result<Self, Failure> {
        let unavailable = || retryable("LUA_LSM_MOUNT_VIEW_UNAVAILABLE");
        let root = rustix::fs::open(
            proc.join("root"),
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| unavailable())?;
        let mounts = mounts(&fs::read(proc.join("mountinfo")).map_err(|_| unavailable())?)
            .ok_or_else(unavailable)?;
        Ok(Self { root, mounts })
    }

    fn checked_type(&self, fd: &OwnedFd) -> Result<FileType, Failure> {
        let required = StatxFlags::MNT_ID | StatxFlags::TYPE;
        let stat = rustix::fs::statx(fd, "", AtFlags::EMPTY_PATH, required)
            .map_err(|e| target_error(e.into()))?;
        if stat.stx_mask & required.bits() != required.bits() {
            return Err(retryable("LUA_LSM_MOUNT_VIEW_UNAVAILABLE"));
        }
        let containing = self
            .mounts
            .iter()
            .find(|m| m.id == stat.stx_mnt_id)
            .ok_or_else(|| retryable("LUA_LSM_MOUNT_VIEW_UNAVAILABLE"))?;
        containing.check_mapping_identity()?;
        Ok(FileType::from_raw_mode(u32::from(stat.stx_mode)))
    }

    fn check_mounts(&self, path: &Path, tree: bool) -> Result<(), Failure> {
        // Resolve absolute symlinks against this process's root. For a future
        // target its nearest existing ancestor determines the mount.
        for ancestor in path.ancestors() {
            let open = |flags| {
                rustix::fs::openat2(
                    &self.root,
                    ancestor,
                    OFlags::PATH | OFlags::CLOEXEC | flags,
                    Mode::empty(),
                    ResolveFlags::IN_ROOT,
                )
            };
            // Entry hooks also protect a final symlink itself. A symlink in a
            // missing target's parent path only selects the backing directory.
            let flags = if ancestor == path {
                OFlags::NOFOLLOW
            } else {
                OFlags::empty()
            };
            let mut fd = match open(flags) {
                Ok(fd) => fd,
                Err(rustix::io::Errno::NOENT) => continue,
                Err(error) => return Err(target_error(error.into())),
            };
            if self.checked_type(&fd)? == FileType::Symlink {
                fd = open(OFlags::empty()).map_err(|e| target_error(e.into()))?;
                self.checked_type(&fd)?;
            }
            if tree && ancestor == path {
                // mountinfo paths are relative to the subject's root. Resolve the
                // directory first so symlink spellings include the same submounts.
                let fd_path = |fd: &OwnedFd| {
                    fs::read_link(format!("/proc/self/fd/{}", fd.as_raw_fd())).map_err(target_error)
                };
                let root = fd_path(&self.root)?;
                let directory = fd_path(&fd)?;
                let relative = directory
                    .strip_prefix(root)
                    .map_err(|_| retryable("LUA_LSM_MOUNT_VIEW_UNAVAILABLE"))?;
                let canonical = Path::new("/").join(relative);
                for mount in &self.mounts {
                    if mount.point != canonical && mount.point.starts_with(&canonical) {
                        mount.check_mapping_identity()?;
                    }
                }
            }
            return Ok(());
        }
        Err(retryable("LUA_LSM_MOUNT_VIEW_UNAVAILABLE"))
    }
}

fn unescape(field: &[u8]) -> Option<PathBuf> {
    let mut bytes = Vec::with_capacity(field.len());
    let mut iter = field.iter().copied();
    while let Some(b) = iter.next() {
        bytes.push(if b == b'\\' {
            match [iter.next()?, iter.next()?, iter.next()?] {
                [b'0', b'4', b'0'] => b' ',
                [b'0', b'1', b'1'] => b'\t',
                [b'0', b'1', b'2'] => b'\n',
                [b'1', b'3', b'4'] => b'\\',
                _ => return None,
            }
        } else {
            b
        });
    }
    Some(OsString::from_vec(bytes).into())
}

fn mounts(bytes: &[u8]) -> Option<Vec<Mount>> {
    bytes
        .split(|&b| b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.split(|&b| b == b' ');
            let id = std::str::from_utf8(fields.next()?).ok()?.parse().ok()?;
            let device = fields.nth(1)?.to_vec();
            let root = unescape(fields.next()?)?;
            let point = unescape(fields.next()?)?;
            fields.find(|&field| field == b"-")?;
            // mountinfo may append a subtype (e.g. fuse.sshfs); Lua exposes s_type->name.
            let filesystem = std::str::from_utf8(fields.next()?)
                .ok()?
                .split('.')
                .next()?
                .to_owned();
            Some(Mount {
                id,
                device,
                root,
                point,
                filesystem,
            })
        })
        .collect()
}

fn joined(base: &Path, suffix: &Path) -> PathBuf {
    base.join(suffix).components().collect()
}

// Resolve an authored symlink even when one or more trailing components do not exist.
fn canonical_target(target: &Path, tree: bool) -> Result<PathBuf, Failure> {
    for ancestor in target.ancestors() {
        if let Some(resolved) = resolve_target(ancestor)? {
            let meta = fs::metadata(&resolved).map_err(target_error)?;
            let directory = tree || ancestor != target;
            if (directory && !meta.is_dir()) || (!directory && !meta.is_file()) {
                return Err(rejected("LUA_LSM_TARGET_TYPE_MISMATCH"));
            }
            return Ok(joined(
                &resolved,
                target
                    .strip_prefix(ancestor)
                    .map_err(|_| rejected("LUA_LSM_INVALID_PATH"))?,
            ));
        }
    }
    Err(retryable("LUA_LSM_TARGET_CHANGED"))
}

fn views_for_target(
    authored: &Path,
    canonical: &Path,
    tree: bool,
    host: &[Mount],
    subject: &[Mount],
) -> Result<BTreeSet<PathView>, Failure> {
    let mut views = BTreeSet::new();
    let containing = host
        .iter()
        .filter(|m| canonical.starts_with(&m.point))
        .max_by_key(|m| m.point.as_os_str().len())
        .ok_or_else(|| retryable("LUA_LSM_MOUNT_VIEW_UNAVAILABLE"))?;
    // A rename above a nested mount operates on the enclosing filesystem.
    for outer in host.iter().filter(|outer| {
        outer.point != containing.point && containing.point.starts_with(&outer.point)
    }) {
        if let Ok(tail) = containing.point.strip_prefix(&outer.point) {
            views.insert(PathView {
                base: joined(&outer.root, tail),
                logical: None,
                filesystem: Some(outer.filesystem.clone()),
            });
        }
    }
    if canonical != authored {
        views.insert(PathView {
            base: canonical.into(),
            logical: Some(authored.into()),
            filesystem: None,
        });
    }
    for mount in std::iter::once(containing).chain(
        host.iter()
            .filter(|m| tree && m.point != canonical && m.point.starts_with(canonical)),
    ) {
        let (raw, logical) = if let Ok(tail) = canonical.strip_prefix(&mount.point) {
            (joined(&mount.root, tail), authored.to_owned())
        } else {
            (
                mount.root.clone(),
                joined(
                    authored,
                    mount
                        .point
                        .strip_prefix(canonical)
                        .map_err(|_| rejected("LUA_LSM_INVALID_PATH"))?,
                ),
            )
        };
        views.insert(PathView {
            base: raw.clone(),
            logical: Some(logical.clone()),
            filesystem: Some(mount.filesystem.clone()),
        });
        for alias in subject.iter().filter(|alias| alias.device == mount.device) {
            let (base, logical) = if let Ok(tail) = raw.strip_prefix(&alias.root) {
                (joined(&alias.point, tail), logical.clone())
            } else if let Ok(tail) = alias.root.strip_prefix(&raw) {
                if !tree {
                    continue;
                }
                (alias.point.clone(), joined(&logical, tail))
            } else {
                continue;
            };
            if base != logical {
                views.insert(PathView {
                    base,
                    logical: Some(logical),
                    filesystem: None,
                });
            }
        }
    }
    Ok(views)
}

pub(super) fn path_views(plan: &LuaLsmPlan) -> Result<Vec<BTreeSet<PathView>>, Failure> {
    let host = MountView::read(Path::new("/proc/self"))?;
    // Updates can transfer surviving members after their original root exits.
    let subject = if super::process_tree::same_process(&plan.process)? {
        let view = MountView::read(&PathBuf::from(format!("/proc/{}", plan.process.pid)))?;
        verify_process(&plan.process)?;
        Some(view)
    } else {
        None
    };
    plan.policy
        .targets
        .iter()
        .map(|target| {
            let authored = Path::new(&target.path);
            let tree = target.kind == FileProtectionTargetKind::DirectoryTree;
            let canonical = canonical_target(authored, tree)?;
            host.check_mounts(authored, tree)?;
            let subject_mounts = subject.as_ref().map_or(&host.mounts, |view| &view.mounts);
            let mut views =
                views_for_target(authored, &canonical, tree, &host.mounts, subject_mounts)?;
            if canonical != authored {
                // Unlink and overwrite operate on the symlink entry itself.
                views.extend(views_for_target(
                    authored,
                    authored,
                    tree,
                    &host.mounts,
                    subject_mounts,
                )?);
            }
            if let Some(subject) = &subject {
                subject.check_mounts(authored, tree)?;
                for view in views.iter().filter(|view| view.filesystem.is_none()) {
                    subject.check_mounts(&view.base, tree)?;
                }
            }
            Ok(views)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_filesystems_are_rejected_on_the_resolved_mount_and_submounts() {
        let fixture = tempfile::tempdir().unwrap();
        fs::create_dir(fixture.path().join("tree")).unwrap();
        fs::write(fixture.path().join("file"), b"data").unwrap();
        std::os::unix::fs::symlink("/tree", fixture.path().join("alias")).unwrap();
        let root = rustix::fs::open(fixture.path(), OFlags::PATH, Mode::empty()).unwrap();
        let id = rustix::fs::statx(&root, "", AtFlags::EMPTY_PATH, StatxFlags::MNT_ID)
            .unwrap()
            .stx_mnt_id;
        let mut view = MountView {
            root,
            mounts: Vec::new(),
        };
        for (filesystem, supported, code) in [
            ("overlay", "tmpfs", "LUA_LSM_OVERLAYFS_UNSUPPORTED"),
            ("overlayfs", "ext4", "LUA_LSM_OVERLAYFS_UNSUPPORTED"),
            ("btrfs", "ext4", "LUA_LSM_BTRFS_UNSUPPORTED"),
        ] {
            let table = format!("{id} 0 0:1 / / rw - {filesystem} none rw\n");
            view.mounts = mounts(table.as_bytes()).unwrap();
            for (path, tree) in [
                ("/file", false),
                ("/tree", true),
                ("/future/secret", false),
                ("/future/tree", true),
            ] {
                assert_eq!(
                    view.check_mounts(Path::new(path), tree).unwrap_err().code,
                    code,
                    "{path}"
                );
            }

            // The target's mount ID distinguishes its supported mount from its parent.
            let table = format!(
                "1 0 0:9 / / rw - {filesystem} none rw\n{id} 1 0:1 / / rw - {supported} none rw\n3 {id} 0:3 / /tree/sub rw - {filesystem} none rw\n"
            );
            view.mounts = mounts(table.as_bytes()).unwrap();
            assert!(view.check_mounts(Path::new("/file"), false).is_ok());
            assert!(view.check_mounts(Path::new("/future/tree"), true).is_ok());
            for tree in ["/tree", "/alias"] {
                assert_eq!(
                    view.check_mounts(Path::new(tree), true).unwrap_err().code,
                    code
                );
            }
        }
    }

    #[test]
    fn mount_relative_entries_and_visible_bind_aliases_project_to_authored_paths() {
        let host = mounts(b"1 0 8:1 / / rw - ext4 disk rw\n2 1 0:2 / /ram rw - tmpfs tmpfs rw\n3 2 0:3 / /ram/data/sub rw - fuse.test source rw\n").unwrap();
        let subject = mounts(
            br"5 1 0:2 /data /alias\040dir rw - tmpfs tmpfs rw
6 1 0:4 /data /different rw - tmpfs tmpfs rw
",
        )
        .unwrap();
        let views = views_for_target(
            Path::new("/configured"),
            Path::new("/ram/data"),
            true,
            &host,
            &subject,
        )
        .unwrap();
        let contains = |base: &str, logical: &str, fs: Option<&str>| {
            views.contains(&PathView {
                base: base.into(),
                logical: Some(logical.into()),
                filesystem: fs.map(str::to_owned),
            })
        };
        assert!(views.contains(&PathView {
            base: "/ram".into(),
            logical: None,
            filesystem: Some("ext4".into())
        }));
        assert!(contains("/data", "/configured", Some("tmpfs")));
        assert!(contains("/", "/configured/sub", Some("fuse")));
        assert!(contains("/ram/data", "/configured", None));
        assert!(contains("/alias dir", "/configured", None));
        assert!(!contains("/different", "/configured", None));
    }
}
