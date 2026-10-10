//! Reusable policy rules, independent of subject assignment and target backends.

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
    /// Ordered rules; backend support is checked during Binding translation.
    pub rules: Vec<Rule>,
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
        if self.spec_version != "0.1" {
            return Err(ValidationError::new("specVersion", "only 0.1 is supported"));
        }
        if self.rules.is_empty() || self.rules.len() > 1024 {
            return Err(ValidationError::new("rules", "requires 1..1024 rules"));
        }
        validate_text(self.description.as_deref(), "description")?;
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
