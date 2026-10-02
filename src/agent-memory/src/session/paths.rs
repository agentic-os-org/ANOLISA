use std::path::{Component, Path, PathBuf};

use super::service::SessionLogService;
use crate::error::{MemoryError, Result};

/// Resolve a path inside `<session>/scratch/`. Same defense-in-depth rules as
/// the mount sandbox: relative only, no `..`, no `.`, no null bytes; no access
/// to the session root (meta.toml / log.jsonl) — only `scratch/`.
pub fn resolve_in_scratch(session: &SessionLogService, raw: &str) -> Result<PathBuf> {
    if raw.is_empty() {
        return Err(MemoryError::InvalidArgument("empty session path".into()));
    }
    let p = Path::new(raw);
    if p.is_absolute() {
        return Err(MemoryError::PathOutsideMount(raw.into()));
    }

    for comp in p.components() {
        match comp {
            Component::Normal(seg) => {
                let s = seg.to_str().ok_or_else(|| {
                    MemoryError::InvalidArgument(format!("non-utf8 segment in '{raw}'"))
                })?;
                if s.is_empty() || s.contains('\0') {
                    return Err(MemoryError::InvalidArgument(format!(
                        "invalid segment in '{raw}'"
                    )));
                }
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

    let joined = session.scratch_root().join(p);

    if joined.exists() {
        let canon = joined.canonicalize()?;
        let scratch_canon = session.scratch_root().canonicalize()?;
        if !canon.starts_with(&scratch_canon) {
            return Err(MemoryError::PathOutsideMount(raw.into()));
        }
    }

    Ok(joined)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{SessionId, SessionLogService};
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, SessionLogService) {
        let tmp = tempdir().unwrap();
        let svc = SessionLogService::start(
            tmp.path(),
            SessionId::from_string("ses_test").unwrap(),
            "alice",
            Some("test"),
            "user-alice",
            None,
        )
        .unwrap();
        (tmp, svc)
    }

    #[test]
    fn rejects_absolute() {
        let (_t, s) = setup();
        assert!(matches!(
            resolve_in_scratch(&s, "/etc/passwd"),
            Err(MemoryError::PathOutsideMount(_))
        ));
    }

    #[test]
    fn rejects_parent() {
        let (_t, s) = setup();
        assert!(matches!(
            resolve_in_scratch(&s, "../meta.toml"),
            Err(MemoryError::PathOutsideMount(_))
        ));
    }

    #[test]
    fn allows_normal() {
        let (_t, s) = setup();
        let p = resolve_in_scratch(&s, "draft/note.md").unwrap();
        assert!(p.starts_with(s.scratch_root()));
    }
}
