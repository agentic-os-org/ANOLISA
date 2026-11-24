//! Resolve policy roots into the paths exposed by Linux file and dentry hooks.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use asc_file_policy::FileProtectionTargetKind;
use asc_lua_lsm_protocol::{LuaLsmPlan, PathView};
use asc_policy_types::target::Failure;

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
    device: Vec<u8>,
    root: PathBuf,
    point: PathBuf,
    filesystem: String,
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
            let device = fields.nth(2)?.to_vec();
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
    let unavailable = || retryable("LUA_LSM_MOUNT_VIEW_UNAVAILABLE");
    let host = mounts(&fs::read("/proc/self/mountinfo").map_err(|_| unavailable())?)
        .ok_or_else(unavailable)?;
    // Updates can transfer surviving members after their original root exits.
    let subject = if verify_process(&plan.process).is_ok() {
        let bytes =
            fs::read(format!("/proc/{}/mountinfo", plan.process.pid)).map_err(|_| unavailable())?;
        verify_process(&plan.process)?;
        Some(mounts(&bytes).ok_or_else(unavailable)?)
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
            let subject = subject.as_deref().unwrap_or(&host);
            let mut views = views_for_target(authored, &canonical, tree, &host, subject)?;
            if canonical != authored {
                // Unlink and overwrite operate on the symlink entry itself.
                views.extend(views_for_target(authored, authored, tree, &host, subject)?);
            }
            Ok(views)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
