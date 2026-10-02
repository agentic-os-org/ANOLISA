use std::fmt;
use std::path::PathBuf;

/// Skill priority level. Lower ordinal = higher priority.
/// Project-level skills override Custom, Custom overrides User,
/// User overrides Extension, Extension overrides System.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum SkillLevel {
    Project,
    Custom,
    User,
    Extension,
    System,
}

impl fmt::Display for SkillLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SkillLevel::Project => write!(f, "project"),
            SkillLevel::Custom => write!(f, "custom"),
            SkillLevel::User => write!(f, "user"),
            SkillLevel::Extension => write!(f, "extension"),
            SkillLevel::System => write!(f, "system"),
        }
    }
}

impl SkillLevel {
    /// Returns all levels in priority order (highest first).
    pub fn all() -> &'static [SkillLevel] {
        &[
            SkillLevel::Project,
            SkillLevel::Custom,
            SkillLevel::User,
            SkillLevel::Extension,
            SkillLevel::System,
        ]
    }
}

/// A fully resolved skill definition, analogous to copilot-shell's `SkillConfig`.
#[derive(Debug, Clone)]
pub struct SkillConfig {
    /// Unique name of the skill (e.g. "code-review").
    pub name: String,
    /// Human-readable description shown in skill list.
    pub description: String,
    /// Tools the skill is allowed to use (empty = all tools).
    #[allow(dead_code)]
    pub allowed_tools: Vec<String>,
    /// The prompt body (everything after the YAML frontmatter).
    pub body: String,
    /// Which level this skill was loaded from.
    pub level: SkillLevel,
    /// Absolute path to the skill file (SKILL.md or flat .md).
    #[allow(dead_code)]
    pub file_path: PathBuf,
    /// Base directory for relative path resolution inside the skill prompt.
    pub base_dir: PathBuf,
}
