pub mod paths;

use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::error::{MemoryError, Result};
use crate::mount::MountStrategy;

/// Namespace kind. P0+P1 only uses `User`; `Agent` / `Team` are reserved for P2.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NsKind {
    User,
    Agent,
    Team,
}

impl NsKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            NsKind::User => "user",
            NsKind::Agent => "agent",
            NsKind::Team => "team",
        }
    }
}

/// Logical namespace identifying who owns this memory.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Namespace {
    pub kind: NsKind,
    pub id: String,
}

impl Namespace {
    /// Build a `User` namespace, rejecting ids that would break out of the
    /// base directory (path separators, `..`, NUL, control characters).
    /// `user_id` is interpolated into the on-disk directory name as
    /// `user-<id>`, so an unvalidated value would let any caller who can
    /// set `USER_ID` env land outside `<base>`.
    pub fn user(id: impl Into<String>) -> Result<Self> {
        let id = id.into();
        validate_user_id(&id)?;
        Ok(Self {
            kind: NsKind::User,
            id,
        })
    }

    /// Folder name used under the base directory: `<kind>-<id>`.
    pub fn dir_name(&self) -> String {
        format!("{}-{}", self.kind.as_str(), self.id)
    }
}

/// Concrete on-disk mount of a namespace.
pub struct MountPoint {
    pub ns: Namespace,
    pub root: PathBuf,
    pub meta_dir: PathBuf,
    /// O_PATH fd on `root`, opened once at construction. Tools that read
    /// or write file content pass `root_fd.as_fd()` to `safe_fs::*` so
    /// every open is anchored against this fd with RESOLVE_BENEATH —
    /// closing the symlink-TOCTOU window that `resolve_path`'s string
    /// check alone could not.
    pub root_fd: Arc<OwnedFd>,
}

/// Path segments that tools may never write/read/edit as the first component.
/// `.anolisa` is the OS-managed meta directory. Any `.git*` prefix is
/// version-control infrastructure — `.gitignore`, `.gitattributes`,
/// `.gitmodules` etc. A model that overwrites `.gitattributes` can break
/// diff/merge behavior; one that overwrites `.gitignore` can neutralize the
/// `.anolisa/` exclusion and cause the next auto-commit to begin tracking
/// audit logs, snapshots, and the FTS DB.
pub(crate) const RESERVED_FIRST_SEGMENTS: &[&str] = &[".anolisa", ".git", ".gitignore"];

/// Returns true when `seg` is a reserved first path segment. Matches the
/// explicit list plus any `.git*` prefix that is version-control infrastructure.
/// `.github` and similar non-VCS `.git*` names are NOT blocked — they are
/// user content, not git internals.
pub(crate) fn is_reserved_first_segment(seg: &str) -> bool {
    RESERVED_FIRST_SEGMENTS.contains(&seg)
        || (seg.starts_with(".git") && !seg.starts_with(".github"))
}

pub(crate) const README_TEXT: &str = r#"# Agent Memory Store

This directory is your persistent memory, mounted by Anolisa.

You can freely create files and folders here using the `mem_*` tools.
There is no schema — organize as you see fit.

The `.anolisa/` subdirectory is reserved for OS-managed metadata
(audit log, manifest) and is not writable by tools. Any `.git*`
prefix (`.git/`, `.gitignore`, `.gitattributes`, `.gitmodules`)
is also reserved to protect version-control integrity.

Suggested layout (entirely optional):
- README.md         (this file — feel free to overwrite)
- notes/            (free-form notes)
- strategies/       (long-form playbooks)
- observations.md   (current state of the world)
"#;

impl MountPoint {
    /// Construct a MountPoint by delegating root resolution to a strategy.
    /// `base` is the configured base dir (e.g. `~/.anolisa/memory`); the
    /// strategy may either return `<base>/<ns>/` directly or perform mount
    /// syscalls and return a different absolute path (e.g. `/mnt/memory/<ns>/`).
    pub fn ensure_with(ns: Namespace, base: &Path, strategy: &dyn MountStrategy) -> Result<Self> {
        let root = strategy.ensure(&ns, base)?;
        let meta_dir = root.join(RESERVED_FIRST_SEGMENTS[0]);
        let root_fd = Arc::new(crate::safe_fs::open_root(&root)?);
        Ok(Self {
            ns,
            root,
            meta_dir,
            root_fd,
        })
    }

    /// Backwards-compatible: equivalent to `ensure_with(ns, base, &UserlandMount)`.
    /// Used by tests and the P0+P1 default code path.
    pub fn ensure(ns: Namespace, base: &Path) -> Result<Self> {
        Self::ensure_with(ns, base, &crate::mount::userland::UserlandMount)
    }

    pub fn audit_log_path(&self) -> PathBuf {
        self.meta_dir.join("audit.log")
    }

    pub fn meta_dir_name(&self) -> &'static str {
        RESERVED_FIRST_SEGMENTS[0]
    }
}

/// Convenience: validate that a name segment doesn't contain forbidden chars.
pub(crate) fn validate_segment(seg: &str) -> Result<()> {
    if seg.is_empty() {
        return Err(MemoryError::InvalidArgument("empty path segment".into()));
    }
    if seg == "." || seg == ".." {
        return Err(MemoryError::InvalidArgument(format!(
            "forbidden segment: {seg}"
        )));
    }
    if seg.contains('\0') {
        return Err(MemoryError::InvalidArgument("null byte in path".into()));
    }
    Ok(())
}

/// Validate an identifier that will be interpolated into an on-disk path
/// (e.g. `user_id`, `session_id`). Stricter than `validate_segment`: rejects
/// any path-separator-ish character (`/`, `\`), any `..` substring (so
/// even `foo..bar` is refused, since the dir name `user-foo..bar` is one
/// segment but the substring still suggests traversal intent), control
/// characters, and lengths beyond 128 bytes (NAME_MAX is 255 but we
/// prefix `user-` / `ses_` and want headroom).
pub fn validate_user_id(id: &str) -> Result<()> {
    if id.is_empty() {
        return Err(MemoryError::InvalidArgument(
            "user_id must not be empty".into(),
        ));
    }
    if id.len() > 128 {
        return Err(MemoryError::InvalidArgument(format!(
            "user_id length {} exceeds 128 bytes",
            id.len()
        )));
    }
    if id.contains('/') || id.contains('\\') {
        return Err(MemoryError::InvalidArgument(format!(
            "user_id '{id}' contains path separator"
        )));
    }
    if id.contains("..") {
        return Err(MemoryError::InvalidArgument(format!(
            "user_id '{id}' contains '..'"
        )));
    }
    if id.chars().any(|c| c.is_control()) {
        return Err(MemoryError::InvalidArgument(format!(
            "user_id '{id}' contains control character"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_accepts_normal_ids() {
        assert!(Namespace::user("alice").is_ok());
        assert!(Namespace::user("1000").is_ok());
        assert!(Namespace::user("me.local").is_ok());
        assert!(Namespace::user("user_42").is_ok());
    }

    #[test]
    fn user_rejects_traversal() {
        assert!(matches!(
            Namespace::user("../escape"),
            Err(MemoryError::InvalidArgument(_))
        ));
        assert!(matches!(
            Namespace::user("a/b"),
            Err(MemoryError::InvalidArgument(_))
        ));
        assert!(matches!(
            Namespace::user("a..b"),
            Err(MemoryError::InvalidArgument(_))
        ));
        assert!(matches!(
            Namespace::user("a\\b"),
            Err(MemoryError::InvalidArgument(_))
        ));
        assert!(matches!(
            Namespace::user("a\0b"),
            Err(MemoryError::InvalidArgument(_))
        ));
        assert!(matches!(
            Namespace::user(""),
            Err(MemoryError::InvalidArgument(_))
        ));
        assert!(matches!(
            Namespace::user("a\nb"),
            Err(MemoryError::InvalidArgument(_))
        ));
    }
}
