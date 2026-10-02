//! Path classification and parsing helpers for the FUSE layer.
//!
//! Pure, dependency-free logic shared by the FUSE callbacks and the
//! mount/discover code: [`PathType`] is the typed view of every FUSE
//! path SkillFS observes, [`parse_path`] (with the L1-inbox companion
//! [`parse_inbox_components`]) is its sole constructor, and
//! [`find_common_path_prefix`] is the parent-prefix helper used by
//! `skill-discover` when summarizing secondary view sources.

use std::path::{Path, PathBuf};

use crate::security::inbox::is_inbox_dir_name;

/// Skill directory layout mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SkillLayout {
    /// Traditional flat layout: `{source}/{skill}/SKILL.md`.
    #[default]
    Flat,
    /// Hermes hub workspace layout.
    ///
    /// Management paths (`.hub/`, `.bundled_manifest`,
    /// `.no-bundled-skills`) are passthrough — no notify, no
    /// activation gating. Skills live at
    /// `{source}/{category}/{skill}/SKILL.md`.
    ///
    /// Supports `--activation-mode file` (current / fallback /
    /// hidden), `--notify-socket` mutation events, and installer
    /// transactions (staging rename, pending install, quiet timeout,
    /// post-publish grace). Incompatible with `--decision-command`
    /// (rejected at startup). Control socket resolver and activation write
    /// methods support full nested Skill ids.
    ///
    /// **Parser model:** purely lexical. Every non-management
    /// top-level entry is classified as `CategoryDir`; every second-
    /// level entry as `NestedSkillDir`. The parser does not probe
    /// for `SKILL.md` — a subdirectory without `SKILL.md` is still
    /// a valid `NestedSkillDir` for POSIX traversal purposes.
    Hermes,
}

/// Well-known Hermes management paths that must be passthrough.
const HERMES_MANAGEMENT_PATHS: &[&str] = &[".hub", ".bundled_manifest", ".no-bundled-skills"];

pub fn is_hermes_management_path(name: &str) -> bool {
    HERMES_MANAGEMENT_PATHS.contains(&name)
}

/// Conservatively auto-detect the skill layout for a source root.
///
/// A source is Hermes only when it carries an unambiguous Hermes hub
/// marker: a `.bundled_manifest` file or a `.hub/` directory. Everything
/// else — including a bare `.no-bundled-skills` sentinel, which a flat
/// workspace can also carry — is treated as [`SkillLayout::Flat`]. This
/// keeps the default safe: a normal flat tree is never misread as Hermes.
pub fn detect_skill_layout(source: &Path) -> SkillLayout {
    if source.join(".bundled_manifest").exists() || source.join(".hub").is_dir() {
        SkillLayout::Hermes
    } else {
        SkillLayout::Flat
    }
}

/// Types of paths in the SkillFS filesystem.
#[derive(Debug, Clone, PartialEq)]
pub enum PathType {
    /// Root directory (/)
    Root,
    /// Skills directory (/skills)
    SkillsDir,
    /// Skill directory (/skills/{skill_name})
    SkillDir { skill_name: String },
    /// SKILL.md file (/skills/{skill_name}/SKILL.md)
    SkillMd { skill_name: String },
    /// Passthrough file/directory (/skills/{skill_name}/{subdir}/...)
    Passthrough {
        skill_name: String,
        relative_path: PathBuf,
    },
    /// L1 inbox virtual root (`/.skillfs-inbox`).
    InboxDir,
    /// L1 inbox skill candidate directory
    /// (`/.skillfs-inbox/{skill_name}`). Maps virtually to the physical
    /// `source/{skill_name}` candidate directory; no physical
    /// `.skillfs-inbox` is ever created on disk.
    InboxSkillDir { skill_name: String },
    /// L1 inbox passthrough leaf
    /// (`/.skillfs-inbox/{skill_name}/{relative_path}`). Maps virtually
    /// to the physical `source/{skill_name}/{relative_path}`. SKILL.md
    /// reads through the inbox are passthrough — only `/skills/<skill>`
    /// runs the compiler.
    InboxPassthrough {
        skill_name: String,
        relative_path: PathBuf,
    },
    /// Hermes management top-level entry (`.hub`, `.bundled_manifest`,
    /// `.no-bundled-skills`). Physical passthrough, not a skill.
    HermesMeta { name: String },
    /// Child path under a Hermes management entry
    /// (e.g. `.hub/some/file`).
    HermesMetaChild {
        name: String,
        relative_path: PathBuf,
    },
    /// Category container directory in Hermes layout
    /// (e.g. `apple/`). Not a skill.
    CategoryDir { category: String },
    /// Physical passthrough of a non-skill child directly under a Hermes
    /// category (e.g. `apple/README.md` or `apple/docs/readme.txt`).
    ///
    /// The lexical parser cannot tell a real nested skill leaf from a
    /// plain file/dir that happens to live under a category, so
    /// [`crate::fs::SkillFs::parse_fuse_path`] probes the source for
    /// `SKILL.md` and rewrites non-skill children into this variant.
    /// `name` is the category component and `relative_path` is everything
    /// below it, so the physical path is `source/name/relative_path` —
    /// identical in shape to [`PathType::HermesMetaChild`], which is why
    /// the two share callback handling. These paths carry no skill
    /// semantics: no compilation, no activation gating, no notify.
    CategoryPassthrough {
        name: String,
        relative_path: PathBuf,
    },
    /// Nested skill directory in Hermes layout
    /// (e.g. `apple/apple-notes/`).
    NestedSkillDir {
        category: String,
        skill_name: String,
    },
    /// SKILL.md under a nested skill in Hermes layout
    /// (e.g. `apple/apple-notes/SKILL.md`).
    NestedSkillMd {
        category: String,
        skill_name: String,
    },
    /// Passthrough path under a nested skill in Hermes layout
    /// (e.g. `apple/apple-notes/scripts/run.sh`).
    NestedPassthrough {
        category: String,
        skill_name: String,
        relative_path: PathBuf,
    },
    /// Unknown/invalid path
    Invalid,
}

/// Parse a path into its type.
///
/// When `in_place` is true the FUSE root IS the skills directory, so
/// paths have no `/skills/` prefix: `/{skill}`, `/{skill}/SKILL.md`, etc.
pub fn parse_path(path: &Path, in_place: bool) -> PathType {
    let components: Vec<_> = path.components().collect();

    // Try the L1 inbox namespace first in both modes. The inbox root is a
    // virtual top-level entry (`/.skillfs-inbox`) that lives alongside
    // `/skills` in normal mode and alongside the in-place skills root in
    // in-place mode.
    if components.len() >= 2 {
        let second = components[1].as_os_str().to_string_lossy();
        if is_inbox_dir_name(&second) {
            return parse_inbox_components(&components);
        }
    }

    if in_place {
        // In-place mode: root == skills dir, no /skills/ prefix.
        match components.as_slice() {
            [] => PathType::SkillsDir,
            [root] if root.as_os_str() == "/" => PathType::SkillsDir,
            [_, skill_name] => PathType::SkillDir {
                skill_name: skill_name.as_os_str().to_string_lossy().to_string(),
            },
            [_, skill_name, file] => {
                let skill_name = skill_name.as_os_str().to_string_lossy().to_string();
                let file_name = file.as_os_str().to_string_lossy();
                if file_name == "SKILL.md" {
                    PathType::SkillMd { skill_name }
                } else {
                    PathType::Passthrough {
                        skill_name,
                        relative_path: PathBuf::from(file.as_os_str()),
                    }
                }
            }
            [_, skill_name, rest @ ..] => {
                let skill_name = skill_name.as_os_str().to_string_lossy().to_string();
                let relative_path: PathBuf = rest.iter().map(|c| c.as_os_str()).collect();
                PathType::Passthrough {
                    skill_name,
                    relative_path,
                }
            }
            _ => PathType::Invalid,
        }
    } else {
        // Normal mode: skills live under /skills/
        match components.as_slice() {
            [] => PathType::Root,
            [root] if root.as_os_str() == "/" => PathType::Root,
            [_, skills] if skills.as_os_str() == "skills" => PathType::SkillsDir,
            [_, skills, skill_name] if skills.as_os_str() == "skills" => PathType::SkillDir {
                skill_name: skill_name.as_os_str().to_string_lossy().to_string(),
            },
            [_, skills, skill_name, file] if skills.as_os_str() == "skills" => {
                let skill_name = skill_name.as_os_str().to_string_lossy().to_string();
                let file_name = file.as_os_str().to_string_lossy();
                if file_name == "SKILL.md" {
                    PathType::SkillMd { skill_name }
                } else {
                    PathType::Passthrough {
                        skill_name,
                        relative_path: PathBuf::from(file.as_os_str()),
                    }
                }
            }
            [_, skills, skill_name, rest @ ..] if skills.as_os_str() == "skills" => {
                let skill_name = skill_name.as_os_str().to_string_lossy().to_string();
                let relative_path: PathBuf = rest.iter().map(|c| c.as_os_str()).collect();
                PathType::Passthrough {
                    skill_name,
                    relative_path,
                }
            }
            _ => PathType::Invalid,
        }
    }
}

/// Parse a path with an explicit layout mode.
///
/// Delegates to [`parse_path`] for [`SkillLayout::Flat`]. For
/// [`SkillLayout::Hermes`] the in-place arm recognises management paths,
/// category directories, and nested skill directories.
pub fn parse_path_with_layout(path: &Path, in_place: bool, layout: SkillLayout) -> PathType {
    if layout == SkillLayout::Flat {
        return parse_path(path, in_place);
    }

    let components: Vec<_> = path.components().collect();

    // Inbox namespace: same in every layout.
    if components.len() >= 2 {
        let second = components[1].as_os_str().to_string_lossy();
        if is_inbox_dir_name(&second) {
            return parse_inbox_components(&components);
        }
    }

    if in_place {
        parse_hermes_in_place(&components)
    } else {
        // Normal (non-in-place) Hermes: skills live under /skills/.
        // The /skills/ prefix is stripped and the remainder is parsed as
        // Hermes layout.
        match components.as_slice() {
            [] => PathType::Root,
            [root] if root.as_os_str() == "/" => PathType::Root,
            [_, skills] if skills.as_os_str() == "skills" => PathType::SkillsDir,
            [_, skills, rest @ ..] if skills.as_os_str() == "skills" => {
                // Synthesise a fake in-place component slice: ["/", rest...]
                let mut synth: Vec<std::path::Component<'_>> = Vec::with_capacity(rest.len() + 1);
                synth.push(components[0]); // "/"
                synth.extend_from_slice(rest);
                parse_hermes_in_place(&synth)
            }
            _ => PathType::Invalid,
        }
    }
}

/// Hermes in-place path classification. The root is the skills
/// directory; top-level entries are either management paths or category
/// directories.
fn parse_hermes_in_place(components: &[std::path::Component<'_>]) -> PathType {
    match components {
        [] => PathType::SkillsDir,
        [root] if root.as_os_str() == "/" => PathType::SkillsDir,
        [_, first] => {
            let name = first.as_os_str().to_string_lossy().to_string();
            if is_hermes_management_path(&name) {
                PathType::HermesMeta { name }
            } else {
                PathType::CategoryDir { category: name }
            }
        }
        [_, first, second] => {
            let first_name = first.as_os_str().to_string_lossy().to_string();
            if is_hermes_management_path(&first_name) {
                return PathType::HermesMetaChild {
                    name: first_name,
                    relative_path: PathBuf::from(second.as_os_str()),
                };
            }
            let second_name = second.as_os_str().to_string_lossy().to_string();
            PathType::NestedSkillDir {
                category: first_name,
                skill_name: second_name,
            }
        }
        [_, first, second, third] => {
            let first_name = first.as_os_str().to_string_lossy().to_string();
            if is_hermes_management_path(&first_name) {
                let relative_path: PathBuf =
                    components[2..].iter().map(|c| c.as_os_str()).collect();
                return PathType::HermesMetaChild {
                    name: first_name,
                    relative_path,
                };
            }
            let second_name = second.as_os_str().to_string_lossy().to_string();
            let third_name = third.as_os_str().to_string_lossy();
            if third_name == "SKILL.md" {
                PathType::NestedSkillMd {
                    category: first_name,
                    skill_name: second_name,
                }
            } else {
                PathType::NestedPassthrough {
                    category: first_name,
                    skill_name: second_name,
                    relative_path: PathBuf::from(third.as_os_str()),
                }
            }
        }
        [_, first, second, rest @ ..] => {
            let first_name = first.as_os_str().to_string_lossy().to_string();
            if is_hermes_management_path(&first_name) {
                let relative_path: PathBuf =
                    components[2..].iter().map(|c| c.as_os_str()).collect();
                return PathType::HermesMetaChild {
                    name: first_name,
                    relative_path,
                };
            }
            let second_name = second.as_os_str().to_string_lossy().to_string();
            let relative_path: PathBuf = rest.iter().map(|c| c.as_os_str()).collect();
            PathType::NestedPassthrough {
                category: first_name,
                skill_name: second_name,
                relative_path,
            }
        }
        _ => PathType::Invalid,
    }
}

/// Parse the `/.skillfs-inbox/...` portion of a FUSE path. Caller must
/// have already verified that `components[1]` matches `INBOX_DIR_NAME`.
/// Mode (in_place / normal) does not affect the inbox layout because the
/// inbox is a virtual top-level entry under the FUSE root in both modes.
pub(crate) fn parse_inbox_components(components: &[std::path::Component<'_>]) -> PathType {
    match components {
        [_, _inbox] => PathType::InboxDir,
        [_, _inbox, skill_name] => PathType::InboxSkillDir {
            skill_name: skill_name.as_os_str().to_string_lossy().to_string(),
        },
        [_, _inbox, skill_name, rest @ ..] => {
            let skill_name = skill_name.as_os_str().to_string_lossy().to_string();
            let relative_path: PathBuf = rest.iter().map(|c| c.as_os_str()).collect();
            PathType::InboxPassthrough {
                skill_name,
                relative_path,
            }
        }
        _ => PathType::Invalid,
    }
}

/// Find the longest common parent-directory prefix across the given file
/// paths.
///
/// Used by `skill-discover` when summarizing secondary view sources so
/// that e.g.
///   `/home/user/skills/github/SKILL.md`
///   `/home/user/skills/discord/SKILL.md`
/// Returns `Some("/home/user/skills")`.
pub(crate) fn find_common_path_prefix(paths: &[std::path::PathBuf]) -> Option<std::path::PathBuf> {
    if paths.is_empty() {
        return None;
    }
    // Work with parent dirs (strip filename component)
    let dirs: Vec<std::path::PathBuf> = paths
        .iter()
        .map(|p| p.parent().map(|d| d.to_path_buf()).unwrap_or_default())
        .collect();

    let first_components: Vec<_> = dirs[0].components().collect();
    let mut common_len = first_components.len();

    for dir in &dirs[1..] {
        let comps: Vec<_> = dir.components().collect();
        let match_len = first_components
            .iter()
            .zip(comps.iter())
            .take_while(|(a, b)| a == b)
            .count();
        common_len = common_len.min(match_len);
    }

    if common_len == 0 {
        return None;
    }

    let prefix: std::path::PathBuf = first_components[..common_len]
        .iter()
        .map(|c| c.as_os_str())
        .collect();
    Some(prefix)
}

/// Check whether a relative path belongs to the skill-discover namespace.
pub(crate) fn is_skill_discover_path(skill_name: &str) -> bool {
    skill_name == "skill-discover"
}
