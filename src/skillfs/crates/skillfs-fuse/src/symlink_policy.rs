//! Pure helpers for reasoning about where a symlink's target lands relative
//! to the SkillFS source tree.
//!
//! This module is **classification only** — no syscalls, no filesystem
//! access, no policy enforcement. It exists so future Skill Security work
//! (Package S0+) can route physical symlinks through a consistent boundary
//! check without coupling to the FUSE callbacks. Callers that need
//! filesystem-validated resolution must perform that separately.

use std::path::{Component, Path, PathBuf};

/// Where a symlink target lands once resolved against the link's parent
/// directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SymlinkTargetClass {
    /// Resolves to a path inside the link's own skill directory.
    SameSkill,
    /// Resolves into a different known skill directory under the same
    /// source. The first path component is reported as `other_skill`.
    CrossSkill { other_skill: String },
    /// Resolves inside the source tree, but not into any known skill
    /// directory (e.g. `skillfs-views.toml`, future `.skill-meta`,
    /// or a not-yet-loaded skill name).
    InsideSourceOutsideSkill,
    /// Resolves outside the source tree, either via an absolute target
    /// that does not start with `source_root` or via `..` components
    /// that escape the source root.
    OutsideSource,
    /// The classifier could not lexically resolve the target — for
    /// example a relative target combined with a non-absolute
    /// `link_parent`, an empty target, or a `..` past `/`.
    RelativeUnknown,
}

/// Classify `raw_target` lexically (no syscalls, no follow).
///
/// * `source_root` — absolute, normalized path to the SkillFS source
///   directory.
/// * `current_skill` — name of the skill that owns the link.
/// * `known_skills` — names of skills currently loaded by the store.
///   Used to distinguish `CrossSkill` from `InsideSourceOutsideSkill`
///   when the target's first component is some other top-level name.
/// * `link_parent` — absolute, normalized path to the directory that
///   contains the link file. Required to resolve relative targets.
/// * `raw_target` — bytes returned by `readlink` on the link.
pub fn classify_symlink_target(
    source_root: &Path,
    current_skill: &str,
    known_skills: &[&str],
    link_parent: &Path,
    raw_target: &Path,
) -> SymlinkTargetClass {
    if raw_target.as_os_str().is_empty() {
        return SymlinkTargetClass::RelativeUnknown;
    }

    let normalized_source = match normalize_lexical(source_root) {
        Some(p) if p.is_absolute() => p,
        _ => return SymlinkTargetClass::RelativeUnknown,
    };

    let resolved = if raw_target.is_absolute() {
        match normalize_lexical(raw_target) {
            Some(p) => p,
            None => return SymlinkTargetClass::OutsideSource,
        }
    } else {
        let normalized_parent = match normalize_lexical(link_parent) {
            Some(p) if p.is_absolute() => p,
            _ => return SymlinkTargetClass::RelativeUnknown,
        };
        match normalize_lexical(&normalized_parent.join(raw_target)) {
            Some(p) => p,
            None => return SymlinkTargetClass::OutsideSource,
        }
    };

    let after_source = match resolved.strip_prefix(&normalized_source) {
        Ok(rel) => rel,
        Err(_) => return SymlinkTargetClass::OutsideSource,
    };

    let mut comps = after_source.components();
    let first = match comps.next() {
        Some(Component::Normal(name)) => name,
        // Resolved path is the source root itself or has no leading
        // Normal component.
        Some(_) | None => return SymlinkTargetClass::InsideSourceOutsideSkill,
    };
    let first_str = first.to_string_lossy();

    if first_str == current_skill {
        SymlinkTargetClass::SameSkill
    } else if known_skills.iter().any(|s| *s == first_str) {
        SymlinkTargetClass::CrossSkill {
            other_skill: first_str.to_string(),
        }
    } else {
        SymlinkTargetClass::InsideSourceOutsideSkill
    }
}

/// Lexical normalization of `.` and `..` components without touching
/// the filesystem. Returns `None` when `..` would escape the absolute
/// root or when an unsupported `Prefix` component (Windows) is hit.
fn normalize_lexical(path: &Path) -> Option<PathBuf> {
    let mut out: Vec<Component> = Vec::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) => return None,
                Some(Component::Prefix(_)) => return None,
                Some(Component::ParentDir) | Some(Component::CurDir) | None => {
                    out.push(Component::ParentDir);
                }
            },
            other => out.push(other),
        }
    }
    Some(out.iter().collect())
}

/// Lexically resolve a relative symlink target inside its own skill and
/// return the resulting path **relative to the skill root** when it
/// stays inside that skill.
///
/// * `link_relative_parent` — the parent directory of the link, expressed
///   relative to the link's skill (e.g. `sub` for a link at
///   `<skill>/sub/link`). May be empty for a link directly under the
///   skill root.
/// * `raw_target` — the user-supplied target. Must be relative; absolute
///   targets are out of scope here (callers should reject them up-front).
///
/// Returns `Some(p)` when the lexical resolution yields a path with at
/// least one `Normal` component and never `..`-escapes the skill root.
/// Returns `None` for absolute targets, empty results, or any `..`
/// chain that walks above the skill root.
///
/// Pure lexical: no filesystem access. Mirrors the resolution
/// `classify_symlink_target` performs after stripping the source prefix
/// and current-skill component, so callers can match the returned path
/// against `.skill-meta` / lifecycle constants directly without a second
/// classifier pass.
pub fn resolve_same_skill_relative(
    link_relative_parent: &Path,
    raw_target: &Path,
) -> Option<PathBuf> {
    if raw_target.is_absolute() {
        return None;
    }
    let combined = link_relative_parent.join(raw_target);
    let mut out: Vec<Component> = Vec::new();
    for c in combined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                // Any `..` past the start of `link_relative_parent`
                // escapes the skill root — caller treats this as
                // not-same-skill.
                _ => return None,
            },
            Component::Normal(_) => out.push(c),
            Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if out.is_empty() {
        return None;
    }
    Some(out.iter().collect())
}

/// Render a symlink classification verdict as a stable, structured label
/// suitable for log fields and audit `detail` strings. Mirrors the variant
/// names but stays in snake_case so log scrapers see a single token.
pub(crate) fn symlink_class_label(class: &SymlinkTargetClass) -> &'static str {
    match class {
        SymlinkTargetClass::SameSkill => "same_skill",
        SymlinkTargetClass::CrossSkill { .. } => "cross_skill",
        SymlinkTargetClass::InsideSourceOutsideSkill => "inside_source_outside_skill",
        SymlinkTargetClass::OutsideSource => "outside_source",
        SymlinkTargetClass::RelativeUnknown => "relative_unknown",
    }
}
