//! Reusable policy rules, independent of subject assignment and target backends.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Validate, ValidationError};

mod condition;
pub use condition::{
    Comparison, Condition, HistoryCondition, HistoryEvent, HistoryLogic, Scalar, Value,
};

/// Reusable policy content; identity and revision belong to `PreparedPolicy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyTemplate {
    /// Rule-language format, independent of the policy content revision.
    pub spec_version: String,
    /// Human-readable context, without effect on matching.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Ordered legacy rules; backend support is checked during Binding translation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
    /// Structured file protections used only by authoring version 0.2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_protection: Option<FileProtectionPolicy>,
}

/// Structured file protections resolved against one fixed mount topology.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileProtectionPolicy {
    /// Mount-topology behavior for all targets.
    pub topology: FileProtectionTopology,
    /// Independently protected paths in authored order.
    pub targets: Vec<FileProtectionTarget>,
}

/// Supported mount-topology behavior for structured file protection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProtectionTopology {
    /// Resolves mount topology once while applying the policy.
    StaticAtApply,
}

/// One literal path and its requested file protections.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileProtectionTarget {
    /// Absolute normalized literal path.
    pub path: String,
    /// Scope interpretation for `path`.
    pub kind: FileProtectionTargetKind,
    /// Independently selected operation protections.
    pub protections: FileProtections,
    /// Descendant entries removed from this target's scope.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclusions: Vec<FileProtectionExclusion>,
}

/// Scope interpretation for a structured target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProtectionTargetKind {
    /// Protects exactly one file entry.
    ExactFile,
    /// Protects one directory and all descendants except exclusions.
    DirectoryTree,
}

/// Operations independently denied for one structured target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileProtections {
    /// Protects the entry from deletion or removal.
    pub delete: bool,
    /// Protects file content from ordinary write operations.
    pub ordinary_write: bool,
}

/// One literal descendant excluded from a directory-tree target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileProtectionExclusion {
    /// Absolute normalized descendant path.
    pub path: String,
    /// Scope interpretation for the excluded entry.
    pub kind: FileProtectionExclusionKind,
}

/// Scope interpretation for a structured exclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileProtectionExclusionKind {
    /// Excludes exactly one directory entry or file.
    ExactEntry,
    /// Excludes one directory and all descendants.
    DirectorySubtree,
}

/// One decision about an operation; Scope selects the subject separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Rule {
    /// Decision when all rule predicates match.
    pub effect: Effect,
    /// Operation domain.
    pub category: Category,
    /// Operation in that domain's vocabulary.
    pub action: Action,
    /// One typed operation target.
    pub target: Resource,
    /// Current-operation predicate; omission means no additional condition.
    #[serde(rename = "where", default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<Condition>,
    /// Historical-event predicate; omission means no history requirement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<HistoryCondition>,
    /// Static explanation, independent of predicate evaluation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub because: Option<String>,
}

/// Decisions with defined policy-language semantics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Deny the matching operation.
    Block,
    /// Allow only when no stronger applicable decision prevents execution.
    Allow,
    /// Wait for confirmation before execution.
    RequireConfirmation,
}

/// Operation domains; each requires a compatible typed target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    /// Filesystem operations.
    File,
    /// Network operations; target serialization is not yet defined.
    Network,
    /// Agent semantic events; target serialization is not yet defined.
    Agenthook,
}

/// Registered file operations; other domains open with their typed resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Read file content.
    Read,
    /// Write content or mutate filesystem entries and attributes.
    Write,
    /// Execute a program.
    Exec,
}

/// Typed operation targets; undefined resource formats are not accepted as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum Resource {
    /// A normalized absolute path or bounded authoring glob.
    File {
        /// Target path pattern.
        path: String,
    },
}

impl Validate for PolicyTemplate {
    fn validate(&self) -> Result<(), ValidationError> {
        validate_text(self.description.as_deref(), "description")?;
        match self.spec_version.as_str() {
            "0.1" => self.validate_legacy_rules(),
            "0.2" => self.validate_structured_file_protection(),
            _ => Err(ValidationError::new(
                "specVersion",
                "only 0.1 and 0.2 are supported",
            )),
        }
    }
}

impl PolicyTemplate {
    fn validate_legacy_rules(&self) -> Result<(), ValidationError> {
        if self.file_protection.is_some() {
            return Err(ValidationError::new(
                "fileProtection",
                "must be absent for specVersion 0.1",
            ));
        }
        if self.rules.is_empty() || self.rules.len() > 1024 {
            return Err(ValidationError::new("rules", "requires 1..1024 rules"));
        }
        let mut nodes = 0;
        for (i, rule) in self.rules.iter().enumerate() {
            let prefix = format!("rules[{i}]");
            validate_operation(rule.category, &rule.target, &prefix)?;
            validate_text(rule.because.as_deref(), &format!("{prefix}.because"))?;
            if let Some(condition) = &rule.condition {
                condition::validate(
                    condition,
                    rule.action,
                    &format!("{prefix}.where"),
                    0,
                    &mut nodes,
                )?;
            }
            if let Some(previous) = &rule.previous {
                condition::validate_history(
                    previous,
                    &format!("{prefix}.previous"),
                    0,
                    &mut nodes,
                )?;
            }
        }
        Ok(())
    }

    fn validate_structured_file_protection(&self) -> Result<(), ValidationError> {
        if !self.rules.is_empty() {
            return Err(ValidationError::new(
                "rules",
                "must be empty for specVersion 0.2",
            ));
        }
        let policy = self.file_protection.as_ref().ok_or_else(|| {
            ValidationError::new("fileProtection", "is required for specVersion 0.2")
        })?;
        validate_file_protection(policy)
    }
}

fn validate_file_protection(policy: &FileProtectionPolicy) -> Result<(), ValidationError> {
    if policy.targets.is_empty() || policy.targets.len() > 64 {
        return Err(ValidationError::new(
            "fileProtection.targets",
            "requires 1..64 targets",
        ));
    }
    let mut target_paths = BTreeMap::new();
    for (target_index, target) in policy.targets.iter().enumerate() {
        let prefix = format!("fileProtection.targets[{target_index}]");
        validate_literal_path(&target.path)
            .map_err(|message| ValidationError::new(format!("{prefix}.path"), message))?;
        if let Some(first_index) = target_paths.insert(target.path.as_str(), target_index) {
            return Err(ValidationError::new(
                format!("{prefix}.path"),
                format!("duplicates target path at index {first_index}"),
            ));
        }
        if !target.protections.delete && !target.protections.ordinary_write {
            return Err(ValidationError::new(
                format!("{prefix}.protections"),
                "requires delete or ordinaryWrite",
            ));
        }
        if target.kind == FileProtectionTargetKind::ExactFile && !target.exclusions.is_empty() {
            return Err(ValidationError::new(
                format!("{prefix}.exclusions"),
                "exact_file targets cannot have exclusions",
            ));
        }
        let mut exclusion_paths = BTreeMap::new();
        for (exclusion_index, exclusion) in target.exclusions.iter().enumerate() {
            let exclusion_prefix = format!("{prefix}.exclusions[{exclusion_index}]");
            validate_literal_path(&exclusion.path).map_err(|message| {
                ValidationError::new(format!("{exclusion_prefix}.path"), message)
            })?;
            if !is_strict_descendant(&target.path, &exclusion.path) {
                return Err(ValidationError::new(
                    format!("{exclusion_prefix}.path"),
                    "must be a strict component descendant of the target path",
                ));
            }
            if let Some((first_index, first_kind)) =
                exclusion_paths.insert(exclusion.path.as_str(), (exclusion_index, exclusion.kind))
            {
                let message = if first_kind == exclusion.kind {
                    format!("duplicates exclusion path at index {first_index}")
                } else {
                    format!("conflicts with exclusion path at index {first_index}")
                };
                return Err(ValidationError::new(
                    format!("{exclusion_prefix}.path"),
                    message,
                ));
            }
        }
    }
    Ok(())
}

fn validate_operation(
    category: Category,
    target: &Resource,
    prefix: &str,
) -> Result<(), ValidationError> {
    if category != Category::File {
        return Err(ValidationError::new(
            format!("{prefix}.target"),
            "resource type must match category; only file targets are defined",
        ));
    }
    let Resource::File { path } = target;
    validate_file_pattern(path)
        .map_err(|message| ValidationError::new(format!("{prefix}.target.path"), message))
}

fn validate_text(value: Option<&str>, path: &str) -> Result<(), ValidationError> {
    if value.is_some_and(|text| text.len() > 4096 || text.contains('\0')) {
        return Err(ValidationError::new(
            path,
            "must not exceed 4096 bytes or contain NUL",
        ));
    }
    Ok(())
}

fn validate_file_pattern(value: &str) -> Result<(), &'static str> {
    if !value.starts_with('/') || value.contains('\0') || value.contains('~') || value.contains('$')
    {
        return Err(
            "path must be absolute and contain no NUL, home expansion, or environment variables",
        );
    }
    if value.len() > 4096 {
        return Err("path exceeds 4096 bytes");
    }
    if value.len() > 1 && value.ends_with('/') {
        return Err("path must not have a trailing separator");
    }
    if value.len() > 1 && value.contains("//") {
        return Err("path contains repeated separators");
    }
    if value
        .split('/')
        .any(|segment| matches!(segment, "." | ".."))
    {
        return Err("path contains a dot segment");
    }
    if value.contains(['*', '?']) {
        if value.contains(['[', ']', '{', '}', '\\']) {
            return Err("glob supports only *, ?, and whole-segment **");
        }
        if value
            .split('/')
            .any(|segment| segment.contains("**") && segment != "**")
        {
            return Err("** must occupy a complete path segment");
        }
    }
    Ok(())
}

fn validate_literal_path(value: &str) -> Result<(), &'static str> {
    if value.is_empty() || !value.starts_with('/') {
        return Err("path must be absolute");
    }
    if value.len() > 4096 {
        return Err("path exceeds 4096 bytes");
    }
    if value.contains('\0') {
        return Err("path must not contain NUL");
    }
    if value.contains(['*', '?', '[', ']', '{', '}', '\\']) {
        return Err("path must be a literal without glob or metacharacters");
    }
    if value.contains("//") {
        return Err("path must not contain repeated separators");
    }
    if value != "/" && value.ends_with('/') {
        return Err("path must not have a trailing separator");
    }
    if value
        .split('/')
        .any(|component| matches!(component, "." | ".."))
    {
        return Err("path must not contain dot components");
    }
    Ok(())
}

fn is_strict_descendant(root: &str, candidate: &str) -> bool {
    if root == "/" {
        return candidate != "/";
    }
    candidate
        .strip_prefix(root)
        .is_some_and(|suffix| suffix.starts_with('/'))
}
