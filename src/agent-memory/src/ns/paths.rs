use std::path::{Component, Path, PathBuf};

use super::{MountPoint, is_reserved_first_segment, validate_segment};
use crate::error::{MemoryError, Result};

/// Resolve a user-supplied relative path against the namespace mount.
///
/// Rules (defense-in-depth):
/// 1. The raw path MUST be relative.
/// 2. No `.` / `..` components are allowed; no null bytes.
/// 3. The first segment must NOT be a reserved name (.anolisa or any .git* prefix).
/// 4. The resolved path must lie under `mount.root`.
/// 5. We do NOT canonicalize unconditionally (the path may not exist yet on
///    write). Symlink escape is prevented at IO-time by canonicalizing
///    already-existing paths and re-checking they still lie under the root.
pub fn resolve_path(mount: &MountPoint, raw: &str) -> Result<PathBuf> {
    if raw.is_empty() {
        return Err(MemoryError::InvalidArgument("empty path".into()));
    }
    let p = Path::new(raw);
    if p.is_absolute() {
        return Err(MemoryError::PathOutsideMount(raw.into()));
    }

    let mut first = true;
    for comp in p.components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg.to_str().ok_or_else(|| {
                    MemoryError::InvalidArgument(format!("non-utf8 path segment in '{raw}'"))
                })?;
                validate_segment(s)?;
                if first && is_reserved_first_segment(s) {
                    return Err(MemoryError::TargetIsReserved(raw.into()));
                }
                first = false;
            }
            Component::CurDir => {
                return Err(MemoryError::InvalidArgument(format!(
                    "'.' not allowed in '{raw}'"
                )));
            }
            Component::ParentDir => return Err(MemoryError::PathOutsideMount(raw.into())),
            Component::RootDir | Component::Prefix(_) => {
                return Err(MemoryError::PathOutsideMount(raw.into()));
            }
        }
    }

    let joined = mount.root.join(p);

    if joined.exists() {
        let canon = joined.canonicalize()?;
        let root_canon = mount.root.canonicalize()?;
        if !canon.starts_with(&root_canon) {
            return Err(MemoryError::PathOutsideMount(raw.into()));
        }
    }

    Ok(joined)
}

/// Same as `resolve_path` but additionally requires the parent directory (if it
/// exists) to lie under the mount root. Used by tools that need to create a
/// new file.
pub fn resolve_for_create(mount: &MountPoint, raw: &str) -> Result<PathBuf> {
    let target = resolve_path(mount, raw)?;
    if let Some(parent) = target.parent() {
        if parent.exists() {
            let canon = parent.canonicalize()?;
            let root_canon = mount.root.canonicalize()?;
            if !canon.starts_with(&root_canon) {
                return Err(MemoryError::PathOutsideMount(raw.into()));
            }
        }
    }
    Ok(target)
}

/// Compute the relative path of `target` under `mount.root`, for display/audit.
pub fn relative_to_mount(mount: &MountPoint, target: &Path) -> String {
    target
        .strip_prefix(&mount.root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| target.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ns::{MountPoint, Namespace, RESERVED_FIRST_SEGMENTS, is_reserved_first_segment};
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, MountPoint) {
        let tmp = tempdir().unwrap();
        let mp = MountPoint::ensure(Namespace::user("alice").unwrap(), tmp.path()).unwrap();
        (tmp, mp)
    }

    #[test]
    fn rejects_absolute() {
        let (_t, mp) = setup();
        assert!(matches!(
            resolve_path(&mp, "/etc/passwd"),
            Err(MemoryError::PathOutsideMount(_))
        ));
    }

    #[test]
    fn rejects_parent_dir() {
        let (_t, mp) = setup();
        assert!(matches!(
            resolve_path(&mp, "../escape"),
            Err(MemoryError::PathOutsideMount(_))
        ));
        assert!(matches!(
            resolve_path(&mp, "notes/../../escape"),
            Err(MemoryError::PathOutsideMount(_))
        ));
    }

    #[test]
    fn rejects_reserved_segments() {
        let (_t, mp) = setup();
        for seg in RESERVED_FIRST_SEGMENTS {
            let path = format!("{seg}/something");
            assert!(matches!(
                resolve_path(&mp, &path),
                Err(MemoryError::TargetIsReserved(_))
            ));
        }
        // Bare reserved name (no subpath) also rejected.
        assert!(matches!(
            resolve_path(&mp, ".gitignore"),
            Err(MemoryError::TargetIsReserved(_))
        ));
        // .git* prefix family also blocked.
        assert!(matches!(
            resolve_path(&mp, ".gitattributes"),
            Err(MemoryError::TargetIsReserved(_))
        ));
        assert!(matches!(
            resolve_path(&mp, ".gitmodules/data"),
            Err(MemoryError::TargetIsReserved(_))
        ));
    }

    #[test]
    fn is_reserved_first_segment_matches_git_family() {
        assert!(is_reserved_first_segment(".anolisa"));
        assert!(is_reserved_first_segment(".git"));
        assert!(is_reserved_first_segment(".gitignore"));
        assert!(is_reserved_first_segment(".gitattributes"));
        assert!(is_reserved_first_segment(".gitmodules"));
        assert!(!is_reserved_first_segment("notes"));
        assert!(!is_reserved_first_segment(".github"));
    }

    #[test]
    fn rejects_empty() {
        let (_t, mp) = setup();
        assert!(matches!(
            resolve_path(&mp, ""),
            Err(MemoryError::InvalidArgument(_))
        ));
    }

    #[test]
    fn allows_normal_relative() {
        let (_t, mp) = setup();
        let p = resolve_path(&mp, "notes/foo.md").unwrap();
        assert!(p.starts_with(&mp.root));
        assert!(p.ends_with("notes/foo.md"));
    }

    #[test]
    fn allows_chinese_filename() {
        let (_t, mp) = setup();
        let p = resolve_path(&mp, "笔记/想法.md").unwrap();
        assert!(p.starts_with(&mp.root));
    }
}
