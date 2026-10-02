use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SkillEntry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SkillEntry {
    /// Skill metadata (from frontmatter)
    pub metadata: SkillMetadata,
    /// Parsed parameters (from ## Parameters section)
    pub parameters: Vec<Parameter>,
    /// Parsed return fields (from ## Returns section)
    pub returns: Vec<ReturnField>,
    /// Raw markdown body (everything after frontmatter)
    pub body: String,
    /// Parse status
    pub parse_status: ParseStatus,
    /// Source file path (absolute)
    pub source_path: PathBuf,
    /// Last modification time of source SKILL.md
    pub last_modified: SystemTime,
}

// ---------------------------------------------------------------------------
// SkillMetadata
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillMetadata {
    /// Skill name — kebab-case, max 64 chars
    #[serde(default)]
    pub name: String,
    /// Short description — max 1024 chars
    #[serde(default)]
    pub description: String,
    /// Semver version string, default "0.0.0"
    #[serde(default = "default_version")]
    pub version: String,
    /// Categorization tags
    #[serde(default)]
    pub tags: Vec<String>,
    /// Whether this skill is active
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Environment requirements (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requires: Option<SkillRequires>,
}

// ---------------------------------------------------------------------------
// SkillRequires
// ---------------------------------------------------------------------------

/// Environment requirements declared in a skill's frontmatter.
///
/// When present, the FUSE layer can filter skills that cannot be executed
/// in the current environment before exposing them to agents.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillRequires {
    /// Commands that must be present in PATH
    #[serde(default)]
    pub commands: Vec<String>,
    /// Supported platforms: "darwin", "linux", "windows"
    #[serde(default)]
    pub platforms: Vec<String>,
    /// Environment variables that must be defined
    #[serde(default)]
    pub env_vars: Vec<String>,
}

fn default_version() -> String {
    "0.0.0".to_string()
}

fn default_enabled() -> bool {
    true
}

impl Default for SkillMetadata {
    fn default() -> Self {
        Self {
            name: String::new(),
            description: String::new(),
            version: default_version(),
            tags: Vec::new(),
            enabled: true,
            requires: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Parameter / ReturnField
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub struct Parameter {
    pub name: String,
    pub param_type: ParamType,
    pub required: bool,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ParamType {
    String,
    Integer,
    Number,
    Boolean,
    Array,
    Object,
}

impl std::fmt::Display for ParamType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ParamType::String => write!(f, "string"),
            ParamType::Integer => write!(f, "integer"),
            ParamType::Number => write!(f, "number"),
            ParamType::Boolean => write!(f, "boolean"),
            ParamType::Array => write!(f, "array"),
            ParamType::Object => write!(f, "object"),
        }
    }
}

impl ParamType {
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "string" | "str" => Some(ParamType::String),
            "integer" | "int" => Some(ParamType::Integer),
            "number" | "float" | "double" => Some(ParamType::Number),
            "boolean" | "bool" => Some(ParamType::Boolean),
            "array" | "list" => Some(ParamType::Array),
            "object" | "map" | "dict" => Some(ParamType::Object),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReturnField {
    pub name: String,
    pub field_type: ParamType,
    pub description: String,
}

// ---------------------------------------------------------------------------
// ParseStatus
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum ParseStatus {
    Ok,
    /// Partial parse succeeded; reason for degradation
    Degraded(String),
    /// Parse failed; error description
    Error(String),
}

impl ParseStatus {
    pub fn is_ok(&self) -> bool {
        matches!(self, ParseStatus::Ok)
    }

    pub fn is_degraded(&self) -> bool {
        matches!(self, ParseStatus::Degraded(_))
    }

    pub fn is_error(&self) -> bool {
        matches!(self, ParseStatus::Error(_))
    }

    pub fn status_str(&self) -> &str {
        match self {
            ParseStatus::Ok => "ok",
            ParseStatus::Degraded(_) => "degraded",
            ParseStatus::Error(_) => "error",
        }
    }

    pub fn message(&self) -> &str {
        match self {
            ParseStatus::Ok => "",
            ParseStatus::Degraded(msg) | ParseStatus::Error(msg) => msg,
        }
    }
}

// ---------------------------------------------------------------------------
// SharedSkillStore type alias
// ---------------------------------------------------------------------------

pub type SharedSkillStore = Arc<RwLock<crate::store::SkillStore>>;

// ---------------------------------------------------------------------------
// CategoryMeta
// ---------------------------------------------------------------------------

/// Metadata for a skill category directory (`_category.yaml`).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CategoryMeta {
    /// Category name (kebab-case)
    pub name: String,
    /// Human-readable description
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone)]
pub struct ParseConfig {
    pub strict: bool,
    pub max_skill_size: usize,
    pub max_skills: usize,
}

impl Default for ParseConfig {
    fn default() -> Self {
        Self {
            strict: false,
            max_skill_size: 1_048_576, // 1MB
            max_skills: 1000,
        }
    }
}
