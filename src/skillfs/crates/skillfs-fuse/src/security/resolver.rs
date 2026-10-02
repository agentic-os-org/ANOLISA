//! Read-only live-source resolver for the control socket.
//!
//! Maps a caller-supplied canonical Skill directory to its physical
//! live/backing source. This module owns path/layout parsing and response
//! construction; the control socket owns authentication, JSON dispatch,
//! and socket lifecycle.
//!
//! [`resolve_live_source`] is O(path depth): it opens the live Skill
//! directory one component at a time with `O_NOFOLLOW` and never scans the
//! whole Skill root. It has no side effects — no scan, manifest build,
//! policy decision, or activation write. It enforces the same Flat / Hermes
//! Skill boundaries as the FUSE layer, so it never reports a phantom Skill
//! at the wrong directory depth.

use std::path::{Component, Path};

use crate::path::SkillLayout;

use super::skill_dir::{SkillDirError, open_verified_skill_dir};

/// Reserved top-level skill name for the synthesized discovery view; it has
/// no physical backing directory.
const SKILL_DISCOVER_NAME: &str = "skill-discover";

/// A structured resolver error. The `code` reuses the control-protocol
/// error-code vocabulary; the control socket maps it 1:1 onto an error
/// response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    pub code: &'static str,
    pub message: String,
}

impl ResolveError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Resolve a canonical Skill directory to its live/backing source.
///
/// Returns the JSON `result` object on success — either `managed = true`
/// with the full mapping and the live directory's `(device, inode)`
/// identity, or `managed = false` when the valid path is outside the
/// managed root — or a structured [`ResolveError`].
///
/// * `canonical_root` — the user-visible root incoming paths are contained
///   against and the relative skill id is derived from.
/// * `live_root` — the physical backing root the live Skill directory is
///   opened under (must be an absolute path).
/// * `layout` — the mount's Skill layout, used to enforce Skill boundaries.
/// * `canonical_skill_dir` — the caller-supplied canonical path.
pub fn resolve_live_source(
    canonical_root: &Path,
    live_root: &Path,
    layout: SkillLayout,
    canonical_skill_dir: &str,
) -> Result<serde_json::Value, ResolveError> {
    // 1. Raw-string lexical validation, BEFORE constructing a `Path` or
    //    doing containment. `Path::components()` normalizes embedded `.`
    //    segments and a NUL byte would only surface at CString time, so an
    //    illegal request must be rejected here rather than silently
    //    becoming a `managed = false` fallback.
    validate_canonical_syntax(canonical_skill_dir)?;

    let requested = Path::new(canonical_skill_dir);

    // 2. Lexical containment against the canonical root. The user path is
    //    never canonicalized; containment is purely component-wise so the
    //    query never crosses the canonical / FUSE / live boundary. A valid
    //    path outside the root is a normal `managed = false`.
    let relative = match requested.strip_prefix(canonical_root) {
        Ok(rel) => rel,
        Err(_) => {
            return Ok(serde_json::json!({
                "managed": false,
                "canonicalSkillDir": canonical_skill_dir,
                "reason": "not_managed",
            }));
        }
    };

    // 3. Derive the relative skill-id components. Every remaining component
    //    is `Normal` (the syntax check rejected `.`/`..`).
    let mut components: Vec<&str> = Vec::new();
    for comp in relative.components() {
        match comp {
            Component::Normal(name) => match name.to_str() {
                Some(s) => components.push(s),
                None => {
                    return Err(ResolveError::new(
                        "invalid_canonical_path",
                        "canonicalSkillDir contains a non-UTF-8 path component",
                    ));
                }
            },
            _ => {
                return Err(ResolveError::new(
                    "invalid_canonical_path",
                    "canonicalSkillDir contains an unexpected relative component",
                ));
            }
        }
    }
    if components.is_empty() {
        return Err(ResolveError::new(
            "invalid_skill_layout",
            "canonicalSkillDir is the canonical root, not a Skill directory",
        ));
    }

    // 4. Reject management / reserved directories. Skill directories and
    //    Hermes categories are never dot-prefixed, so any dot-prefixed
    //    component (`.skill-meta`, `.hub`, `.staging`, `.openclaw-*`, …) is
    //    a managed/reserved location. The synthesized `skill-discover` view
    //    has no physical backing.
    for name in &components {
        if name.starts_with('.') {
            return Err(ResolveError::new(
                "invalid_canonical_path",
                format!("canonicalSkillDir component '{name}' is a managed/reserved directory"),
            ));
        }
    }
    if components[0] == SKILL_DISCOVER_NAME {
        return Err(ResolveError::new(
            "invalid_canonical_path",
            "canonicalSkillDir refers to the synthesized skill-discover view",
        ));
    }

    // 5. Enforce the layout boundary and safely resolve the live directory.
    let leaf = open_verified_skill_dir(live_root, layout, &components)
        .map_err(|error| map_skill_dir_error(error, layout))?;
    let identity = leaf
        .identity()
        .map_err(|error| map_skill_dir_error(error, layout))?;

    let relative_skill_dir = components.join("/");
    let live_skill_dir = live_root.join(&relative_skill_dir);

    Ok(serde_json::json!({
        "managed": true,
        "canonicalSkillDir": canonical_skill_dir,
        "skillId": relative_skill_dir,
        "relativeSkillDir": relative_skill_dir,
        "liveSkillDir": live_skill_dir.to_string_lossy(),
        "identity": {
            "device": identity.dev,
            "inode": identity.ino,
        },
        "transport": "shared_path",
    }))
}

/// Reject illegal canonical paths on the raw string, before any `Path`
/// construction or containment check.
fn validate_canonical_syntax(raw: &str) -> Result<(), ResolveError> {
    if raw.as_bytes().contains(&0) {
        return Err(ResolveError::new(
            "invalid_canonical_path",
            "canonicalSkillDir contains a NUL byte",
        ));
    }
    if !raw.starts_with('/') {
        return Err(ResolveError::new(
            "invalid_canonical_path",
            format!("canonicalSkillDir '{raw}' is not an absolute path"),
        ));
    }
    if raw.contains("//") {
        return Err(ResolveError::new(
            "invalid_canonical_path",
            format!("canonicalSkillDir '{raw}' contains a repeated path separator"),
        ));
    }
    if raw.len() > 1 && raw.ends_with('/') {
        return Err(ResolveError::new(
            "invalid_canonical_path",
            format!("canonicalSkillDir '{raw}' contains a trailing path separator"),
        ));
    }
    for seg in raw.split('/') {
        if seg == "." || seg == ".." {
            return Err(ResolveError::new(
                "invalid_canonical_path",
                format!("canonicalSkillDir '{raw}' contains a '{seg}' segment"),
            ));
        }
    }
    Ok(())
}

fn map_skill_dir_error(error: SkillDirError, layout: SkillLayout) -> ResolveError {
    match error {
        SkillDirError::InvalidDepth => match layout {
            SkillLayout::Flat => ResolveError::new(
                "invalid_skill_layout",
                "flat layout Skills occupy a single directory level",
            ),
            SkillLayout::Hermes => ResolveError::new(
                "invalid_skill_layout",
                "hermes layout Skills occupy at most two directory levels",
            ),
        },
        SkillDirError::RootPathContainsNul => {
            ResolveError::new("live_source_unavailable", "live root path contains NUL")
        }
        SkillDirError::RootOpen(error) => ResolveError::new(
            "live_source_unavailable",
            format!("failed to open live root: {error}"),
        ),
        SkillDirError::ComponentContainsNul => ResolveError::new(
            "invalid_canonical_path",
            "skill path component contains NUL",
        ),
        SkillDirError::ComponentSymlink { component } => ResolveError::new(
            "invalid_canonical_path",
            format!("skill path component '{component}' is a symlink; refusing to follow"),
        ),
        SkillDirError::ComponentMissing { component } => ResolveError::new(
            "skill_not_found",
            format!("skill path component '{component}' does not exist under the managed root"),
        ),
        SkillDirError::ComponentNotDirectory { component } => ResolveError::new(
            "invalid_skill_layout",
            format!("skill path component '{component}' is not a directory"),
        ),
        SkillDirError::ComponentOpen { component, source } => ResolveError::new(
            "live_source_unavailable",
            format!("failed to open skill path component '{component}': {source}"),
        ),
        SkillDirError::NestedBelowTopLevelSkill { parent, child } => ResolveError::new(
            "invalid_skill_layout",
            format!(
                "'{parent}' is a top-level skill, not a category; '{child}' is a \
                 subdirectory, not a nested Skill"
            ),
        ),
        SkillDirError::MarkerInspect(error) => ResolveError::new(
            "live_source_unavailable",
            format!("cannot determine SKILL.md presence: {error}"),
        ),
        SkillDirError::MissingMarker => ResolveError::new(
            "invalid_skill_layout",
            "skill directory is missing a regular SKILL.md",
        ),
        SkillDirError::Identity(error) => ResolveError::new(
            "live_source_unavailable",
            format!("failed to stat live skill directory: {error}"),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn seed_skill(root: &Path, rel: &str) {
        let dir = root.join(rel);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            "---\nname: x\ndescription: y\n---\nbody\n",
        )
        .unwrap();
    }

    fn dir_identity(path: &Path) -> (u64, u64) {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path).unwrap();
        (m.dev(), m.ino())
    }

    fn resolve(
        root: &Path,
        layout: SkillLayout,
        canonical: &Path,
    ) -> Result<serde_json::Value, ResolveError> {
        resolve_live_source(root, root, layout, canonical.to_str().unwrap())
    }

    // ── managed = true ───────────────────────────────────────────────────

    #[test]
    fn flat_skill_managed_true_with_identity() {
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "my-skill");
        let canonical = root.path().join("my-skill");
        let r = resolve(root.path(), SkillLayout::Flat, &canonical).unwrap();
        assert_eq!(r["managed"], true);
        assert_eq!(r["skillId"], "my-skill");
        assert_eq!(r["relativeSkillDir"], "my-skill");
        assert_eq!(r["transport"], "shared_path");
        assert_eq!(r["liveSkillDir"], canonical.to_string_lossy().as_ref());
        let (dev, ino) = dir_identity(&canonical);
        assert_eq!(r["identity"]["device"], dev);
        assert_eq!(r["identity"]["inode"], ino);
    }

    #[test]
    fn hermes_nested_skill_managed_true() {
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "apple/apple-notes");
        let canonical = root.path().join("apple/apple-notes");
        let r = resolve(root.path(), SkillLayout::Hermes, &canonical).unwrap();
        assert_eq!(r["managed"], true);
        assert_eq!(r["skillId"], "apple/apple-notes");
        assert_eq!(r["relativeSkillDir"], "apple/apple-notes");
    }

    #[test]
    fn hermes_mixed_top_level_and_nested() {
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "weather");
        seed_skill(root.path(), "apple/apple-notes");

        let top = resolve(
            root.path(),
            SkillLayout::Hermes,
            &root.path().join("weather"),
        )
        .unwrap();
        assert_eq!(top["skillId"], "weather");

        let nested = resolve(
            root.path(),
            SkillLayout::Hermes,
            &root.path().join("apple/apple-notes"),
        )
        .unwrap();
        assert_eq!(nested["skillId"], "apple/apple-notes");

        // The category itself has no SKILL.md → invalid layout.
        let cat =
            resolve(root.path(), SkillLayout::Hermes, &root.path().join("apple")).unwrap_err();
        assert_eq!(cat.code, "invalid_skill_layout");
    }

    #[test]
    fn identity_comes_from_live_root_not_canonical() {
        let canonical_root = tempfile::tempdir().unwrap();
        let live_root = tempfile::tempdir().unwrap();
        seed_skill(canonical_root.path(), "my-skill");
        seed_skill(live_root.path(), "my-skill");

        let r = resolve_live_source(
            canonical_root.path(),
            live_root.path(),
            SkillLayout::Flat,
            canonical_root.path().join("my-skill").to_str().unwrap(),
        )
        .unwrap();
        let (live_dev, live_ino) = dir_identity(&live_root.path().join("my-skill"));
        let (canon_dev, canon_ino) = dir_identity(&canonical_root.path().join("my-skill"));
        assert_eq!(r["identity"]["device"], live_dev);
        assert_eq!(r["identity"]["inode"], live_ino);
        assert_ne!((canon_dev, canon_ino), (live_dev, live_ino));
    }

    // ── layout boundaries (regression: no phantom skills) ────────────────

    #[test]
    fn flat_rejects_subdirectory_even_with_skill_md() {
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "skill");
        seed_skill(root.path(), "skill/subdir");
        // Flat: a subdirectory is never a Skill, even with its own SKILL.md.
        let err = resolve(
            root.path(),
            SkillLayout::Flat,
            &root.path().join("skill/subdir"),
        )
        .unwrap_err();
        assert_eq!(err.code, "invalid_skill_layout");
        // The top-level skill still resolves.
        let ok = resolve(root.path(), SkillLayout::Flat, &root.path().join("skill")).unwrap();
        assert_eq!(ok["skillId"], "skill");
    }

    #[test]
    fn hermes_rejects_subdir_of_top_level_skill() {
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "top");
        seed_skill(root.path(), "top/sub");
        // `top` is a top-level skill (has SKILL.md), so `top/sub` is a
        // subdirectory, not a nested Skill.
        let err = resolve(
            root.path(),
            SkillLayout::Hermes,
            &root.path().join("top/sub"),
        )
        .unwrap_err();
        assert_eq!(err.code, "invalid_skill_layout");
    }

    #[test]
    fn hermes_rejects_third_level() {
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "a/b/c");
        let err =
            resolve(root.path(), SkillLayout::Hermes, &root.path().join("a/b/c")).unwrap_err();
        assert_eq!(err.code, "invalid_skill_layout");
    }

    #[test]
    fn hermes_unreadable_top_level_skill_md_is_not_a_phantom_category() {
        // Regression: `top` is a top-level skill whose SKILL.md is mode 000.
        // The category check must still see the SKILL.md (via fstatat) and
        // reject `top/child` as a subdirectory, not a nested Skill — a
        // permission error must never be swallowed as "no SKILL.md".
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "top");
        seed_skill(root.path(), "top/child");
        let top_md = root.path().join("top/SKILL.md");
        std::fs::set_permissions(&top_md, std::fs::Permissions::from_mode(0o000)).unwrap();

        let err = resolve(
            root.path(),
            SkillLayout::Hermes,
            &root.path().join("top/child"),
        )
        .unwrap_err();
        assert_eq!(
            err.code, "invalid_skill_layout",
            "unreadable top-level SKILL.md must still block a phantom nested skill"
        );

        // Restore perms so the tempdir can be cleaned up.
        std::fs::set_permissions(&top_md, std::fs::Permissions::from_mode(0o644)).unwrap();
    }

    #[test]
    fn hermes_symlinked_top_level_skill_md_makes_it_a_category() {
        // `top` has a SKILL.md that is a *symlink*, not a regular file. Under
        // the shared no-follow regular-file rule this is not a valid marker,
        // so `top` is a category — exactly as store discovery treats it —
        // and its real nested skill `top/child` resolves (it is NOT a
        // phantom). Querying `top` itself is invalid: no regular marker.
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("top")).unwrap();
        let real_md = root.path().join("real-skill.md");
        std::fs::write(&real_md, "---\nname: x\ndescription: y\n---\n").unwrap();
        std::os::unix::fs::symlink(&real_md, root.path().join("top/SKILL.md")).unwrap();
        seed_skill(root.path(), "top/child");

        let ok = resolve(
            root.path(),
            SkillLayout::Hermes,
            &root.path().join("top/child"),
        )
        .unwrap();
        assert_eq!(ok["managed"], true);
        assert_eq!(ok["skillId"], "top/child");

        let err = resolve(root.path(), SkillLayout::Hermes, &root.path().join("top")).unwrap_err();
        assert_eq!(
            err.code, "invalid_skill_layout",
            "a directory whose only SKILL.md is a symlink has no valid marker"
        );
    }

    #[test]
    fn leaf_skill_md_symlink_is_error_and_not_followed() {
        // A leaf SKILL.md that is a symlink is not a valid marker (no-follow
        // regular-file rule), so the leaf has no marker and resolution fails
        // with invalid_skill_layout — the symlink is never followed.
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("skill")).unwrap();
        let real_md = root.path().join("real.md");
        std::fs::write(&real_md, "---\nname: x\ndescription: y\n---\n").unwrap();
        std::os::unix::fs::symlink(&real_md, root.path().join("skill/SKILL.md")).unwrap();

        let err = resolve(root.path(), SkillLayout::Flat, &root.path().join("skill")).unwrap_err();
        assert_eq!(err.code, "invalid_skill_layout");
        assert!(err.message.contains("regular SKILL.md"));
    }

    #[test]
    fn skill_md_directory_is_error() {
        // A SKILL.md that is a directory (or any non-regular object) is not
        // a valid marker.
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("skill/SKILL.md")).unwrap();
        let err = resolve(root.path(), SkillLayout::Flat, &root.path().join("skill")).unwrap_err();
        assert_eq!(err.code, "invalid_skill_layout");
    }

    // ── managed = false ──────────────────────────────────────────────────

    #[test]
    fn outside_managed_root_is_not_managed() {
        let root = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let r = resolve(root.path(), SkillLayout::Flat, &other.path().join("x")).unwrap();
        assert_eq!(r["managed"], false);
        assert_eq!(r["reason"], "not_managed");
    }

    // ── structured errors ────────────────────────────────────────────────

    #[test]
    fn relative_path_is_error() {
        let root = tempfile::tempdir().unwrap();
        let err = resolve_live_source(root.path(), root.path(), SkillLayout::Flat, "relative/path")
            .unwrap_err();
        assert_eq!(err.code, "invalid_canonical_path");
    }

    #[test]
    fn parent_dir_segment_is_error() {
        let root = tempfile::tempdir().unwrap();
        let bad = format!("{}/../escape", root.path().display());
        let err =
            resolve_live_source(root.path(), root.path(), SkillLayout::Flat, &bad).unwrap_err();
        assert_eq!(err.code, "invalid_canonical_path");
    }

    #[test]
    fn embedded_current_dir_segment_is_error() {
        // Regression: Path::components() would normalize `/root/./skill`, so
        // the raw-string check must reject the `.` segment.
        let root = tempfile::tempdir().unwrap();
        let bad = format!("{}/./my-skill", root.path().display());
        let err =
            resolve_live_source(root.path(), root.path(), SkillLayout::Flat, &bad).unwrap_err();
        assert_eq!(err.code, "invalid_canonical_path");
    }

    #[test]
    fn non_normalized_wire_path_is_error() {
        let root = tempfile::tempdir().unwrap();
        for raw in [
            format!("{}//my-skill", root.path().display()),
            format!("//{}/my-skill", root.path().display()),
            format!("{}/my-skill/", root.path().display()),
        ] {
            let err =
                resolve_live_source(root.path(), root.path(), SkillLayout::Flat, &raw).unwrap_err();
            assert_eq!(err.code, "invalid_canonical_path", "{raw}");
        }
    }

    #[test]
    fn nul_byte_is_error_not_not_managed() {
        // A NUL-containing string outside the root must be a structured
        // error, never a `managed = false` fallback.
        let root = tempfile::tempdir().unwrap();
        let err = resolve_live_source(root.path(), root.path(), SkillLayout::Flat, "/outside/\0/x")
            .unwrap_err();
        assert_eq!(err.code, "invalid_canonical_path");
        assert!(err.message.contains("NUL"));
    }

    #[test]
    fn symlink_escape_is_error() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        seed_skill(outside.path(), "target");
        std::os::unix::fs::symlink(outside.path().join("target"), root.path().join("linkskill"))
            .unwrap();
        let err = resolve(
            root.path(),
            SkillLayout::Flat,
            &root.path().join("linkskill"),
        )
        .unwrap_err();
        assert_eq!(err.code, "invalid_canonical_path");
    }

    #[test]
    fn missing_skill_inside_root_is_error() {
        let root = tempfile::tempdir().unwrap();
        let err = resolve(root.path(), SkillLayout::Flat, &root.path().join("ghost")).unwrap_err();
        assert_eq!(err.code, "skill_not_found");
    }

    #[test]
    fn missing_skill_md_is_error() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("nomd")).unwrap();
        let err = resolve(root.path(), SkillLayout::Flat, &root.path().join("nomd")).unwrap_err();
        assert_eq!(err.code, "invalid_skill_layout");
    }

    #[test]
    fn reserved_directories_are_error() {
        let root = tempfile::tempdir().unwrap();
        for reserved in [".skill-meta", ".staging", ".certified", ".hub"] {
            let err =
                resolve(root.path(), SkillLayout::Flat, &root.path().join(reserved)).unwrap_err();
            assert_eq!(err.code, "invalid_canonical_path", "{reserved}");
        }
    }

    #[test]
    fn skill_discover_is_reserved() {
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "skill-discover");
        let err = resolve(
            root.path(),
            SkillLayout::Flat,
            &root.path().join("skill-discover"),
        )
        .unwrap_err();
        assert_eq!(err.code, "invalid_canonical_path");
    }

    #[test]
    fn canonical_root_itself_is_error() {
        let root = tempfile::tempdir().unwrap();
        let err = resolve(root.path(), SkillLayout::Flat, root.path()).unwrap_err();
        assert_eq!(err.code, "invalid_skill_layout");
    }

    #[test]
    fn resolve_has_no_side_effects() {
        let root = tempfile::tempdir().unwrap();
        seed_skill(root.path(), "my-skill");
        let canonical = root.path().join("my-skill");
        resolve(root.path(), SkillLayout::Flat, &canonical).unwrap();
        assert!(!canonical.join(".skill-meta").exists());
        let entries: Vec<String> = std::fs::read_dir(&canonical)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(entries, vec![PathBuf::from("SKILL.md").to_string_lossy()]);
    }
}
