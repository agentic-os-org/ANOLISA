//! Package L1 install-inbox namespace helpers.
//!
//! L1 reserves the FUSE-visible top-level directory `/.skillfs-inbox` as
//! the install / repair entrance for in-place security mode. The inbox is
//! a **virtual** mapping: writes through `/.skillfs-inbox/<skill>/...`
//! land in the normal physical candidate directory `source/<skill>/...`,
//! while `/skills/<skill>` continues to be controlled by
//! [`crate::security::ActiveSkillResolver`] (current / fallback / hidden).
//!
//! This module only ships the lexical helpers. Path parsing,
//! mount-side wiring, and the install-complete trigger live in
//! [`crate::lib`].
//!
//! Scope (intentionally **out of scope** here):
//!
//! * physical inbox-to-source moves or destructive rollback;
//! * daemon / socket transport for the install-complete signal;
//! * production identity for the installer process;
//! * relaxing `.skill-meta/**` mutation policy — only the trusted ledger
//!   writer remains allowed there.

use std::path::{Component, Path};

/// Canonical name of the inbox virtual root directory.
pub const INBOX_DIR_NAME: &str = ".skillfs-inbox";

/// Maximum length of a skill name component, mirroring
/// `skillfs-core::parser::validate_name`.
pub const INBOX_SKILL_NAME_MAX_LEN: usize = 64;

/// Sentinel relative path under `<inbox>/<skill>` that signals the
/// installer has finished writing the candidate. Writing this file
/// enqueues the existing `scan -> resolve` flow for the owning
/// skill exactly once per debounce window.
pub const INSTALL_COMPLETE_SENTINEL: &str = ".install-complete";

/// Returns `true` when `name` exactly matches the reserved inbox top-level
/// directory name. Case-sensitive; neighbours such as `.skillfs-inbox2`
/// are not reserved.
pub fn is_inbox_dir_name(name: &str) -> bool {
    name == INBOX_DIR_NAME
}

/// Returns `true` when `name` is a syntactically valid inbox skill
/// candidate name.
///
/// The shape rule mirrors `skillfs-core::parser::validate_name` so the
/// inbox cannot expose entries that the loader/parser would refuse to
/// load as skills: kebab-case (`[a-z0-9-]+`), no leading or trailing
/// hyphen, length ≤ 64. This intentionally rejects every dot-prefixed
/// directory the source root may contain — `.git`, `.skill-meta`,
/// `.skillfs-inbox`, `.staging`, `.cache`, etc. — as well as
/// uppercase / underscore / dotted names that the store would not
/// surface as skills either.
///
/// This is a stricter shape check than [`is_inbox_dir_name`] (which is
/// only the inbox root reservation) and is the canonical predicate the
/// FUSE layer uses to admit a top-level segment under
/// `/.skillfs-inbox/<name>`.
pub fn is_valid_inbox_skill_name(name: &str) -> bool {
    if name.is_empty() || name.len() > INBOX_SKILL_NAME_MAX_LEN {
        return false;
    }
    if name.starts_with('-') || name.ends_with('-') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Returns `true` when `relative_path` is exactly the install-complete
/// sentinel at the top of a skill candidate directory.
///
/// `relative_path` is interpreted as the path **inside** an inbox skill
/// directory, i.e. the `relative_path` of an `InboxPassthrough` parsed
/// path. Sub-directory placements such as
/// `scripts/.install-complete` are intentionally not recognized; the
/// sentinel must sit directly under `<inbox>/<skill>/`.
pub fn is_install_complete_path(relative_path: &Path) -> bool {
    let mut components = relative_path.components();
    let first = match components.next() {
        Some(Component::Normal(name)) => name,
        _ => return false,
    };
    if components.next().is_some() {
        return false;
    }
    first == INSTALL_COMPLETE_SENTINEL
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inbox_name_matches_exact() {
        assert!(is_inbox_dir_name(INBOX_DIR_NAME));
        assert!(!is_inbox_dir_name(".skillfs-inbox2"));
        assert!(!is_inbox_dir_name("skillfs-inbox"));
        assert!(!is_inbox_dir_name(""));
    }

    #[test]
    fn install_complete_top_level_only() {
        assert!(is_install_complete_path(Path::new(".install-complete")));
        assert!(!is_install_complete_path(Path::new(
            "scripts/.install-complete"
        )));
        assert!(!is_install_complete_path(Path::new(
            ".install-complete/extra"
        )));
        assert!(!is_install_complete_path(Path::new("install-complete")));
        assert!(!is_install_complete_path(Path::new("")));
    }

    #[test]
    fn valid_inbox_skill_names_match_kebab_case_rule() {
        assert!(is_valid_inbox_skill_name("alpha"));
        assert!(is_valid_inbox_skill_name("demo-weather"));
        assert!(is_valid_inbox_skill_name("a"));
        assert!(is_valid_inbox_skill_name("v2"));
        assert!(is_valid_inbox_skill_name("skill-1-2-3"));
    }

    #[test]
    fn invalid_inbox_skill_names_are_rejected() {
        // Empty / oversized.
        assert!(!is_valid_inbox_skill_name(""));
        assert!(!is_valid_inbox_skill_name(
            &"a".repeat(INBOX_SKILL_NAME_MAX_LEN + 1)
        ));
        // Hidden / dot-prefixed source-root entries.
        assert!(!is_valid_inbox_skill_name(".git"));
        assert!(!is_valid_inbox_skill_name(".skill-meta"));
        assert!(!is_valid_inbox_skill_name(".staging"));
        assert!(!is_valid_inbox_skill_name(".certified"));
        assert!(!is_valid_inbox_skill_name(".quarantine"));
        assert!(!is_valid_inbox_skill_name(".archive"));
        assert!(!is_valid_inbox_skill_name(".skillfs-inbox"));
        assert!(!is_valid_inbox_skill_name(".cache"));
        // Hyphen edges.
        assert!(!is_valid_inbox_skill_name("-alpha"));
        assert!(!is_valid_inbox_skill_name("alpha-"));
        // Non-kebab character classes the store/parser would also
        // refuse: uppercase, underscores, embedded dots, whitespace,
        // path separators.
        assert!(!is_valid_inbox_skill_name("Alpha"));
        assert!(!is_valid_inbox_skill_name("foo_bar"));
        assert!(!is_valid_inbox_skill_name("skillfs-views.toml"));
        assert!(!is_valid_inbox_skill_name("alpha beta"));
        assert!(!is_valid_inbox_skill_name("alpha/beta"));
    }
}
