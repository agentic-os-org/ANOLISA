//! Skill Security path classification helpers.
//!
//! These helpers are pure-lexical (no syscalls, no follow). They centralize
//! the "is this path one of SkillFS's protected metadata locations?" decision
//! so FUSE callbacks and policy implementations agree on the answer.

use std::path::{Component, Path};

/// Reserved metadata directory name under each skill.
pub const SKILL_META_DIR: &str = ".skill-meta";

/// Returns `true` when `relative_path` points at the skill-relative
/// `.skill-meta` directory itself or at any descendant of it.
///
/// `relative_path` is interpreted as the path **inside** a skill directory,
/// i.e. what `parse_path` returns as the `relative_path` of
/// `PathType::Passthrough`. It is matched lexically using path components so
/// neighbours like `.skill-meta2` and same-named subdirectories deeper in
/// the tree (e.g. `docs/.skill-meta`) do not match.
///
/// Examples:
/// * `.skill-meta` → `true`
/// * `.skill-meta/manifest.json` → `true`
/// * `.skill-meta/signatures/root.json` → `true`
/// * `.skill-meta2` → `false`
/// * `docs/.skill-meta` → `false`
/// * `` (empty) → `false`
pub fn is_skill_meta_path(relative_path: &Path) -> bool {
    let mut components = relative_path.components();
    match components.next() {
        Some(Component::Normal(name)) => name == SKILL_META_DIR,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn matches_exact_skill_meta() {
        assert!(is_skill_meta_path(Path::new(".skill-meta")));
    }

    #[test]
    fn matches_nested_under_skill_meta() {
        assert!(is_skill_meta_path(Path::new(".skill-meta/manifest.json")));
        assert!(is_skill_meta_path(Path::new(
            ".skill-meta/signatures/root.json"
        )));
        assert!(is_skill_meta_path(&PathBuf::from(".skill-meta/a/b/c")));
    }

    #[test]
    fn rejects_neighbour_names() {
        assert!(!is_skill_meta_path(Path::new(".skill-meta2")));
        assert!(!is_skill_meta_path(Path::new(".skill-met")));
        assert!(!is_skill_meta_path(Path::new("skill-meta")));
    }

    #[test]
    fn rejects_nested_skill_meta_below_other_dir() {
        assert!(!is_skill_meta_path(Path::new("docs/.skill-meta")));
        assert!(!is_skill_meta_path(Path::new("a/b/.skill-meta/c")));
    }

    #[test]
    fn rejects_empty_relative_path() {
        assert!(!is_skill_meta_path(Path::new("")));
    }
}
