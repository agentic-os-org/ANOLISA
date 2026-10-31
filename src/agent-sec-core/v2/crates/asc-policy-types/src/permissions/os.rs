//! Fixed OS permissions for one sandbox lifetime.

use crate::{Validate, ValidationError};
use serde::{Deserialize, Serialize};

/// Versioned permissions shared by requests and execution backends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsPermissionSet {
    /// Wire version; currently one.
    pub version: u32,
    /// Default-deny filesystem view.
    pub filesystem: FilesystemPermissions,
    /// Mount-level restrictions, independent of invocation authorization.
    pub execution: ExecutionPermissions,
    /// IP networking access.
    pub network: NetworkPermissions,
}

/// Unlisted paths are inaccessible.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemPermissions {
    /// The only supported fallback is deny.
    pub default: FilesystemDefault,
    /// Explicit allows and overriding denies.
    pub rules: Vec<FilesystemRule>,
}

/// Default behavior outside declared mounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilesystemDefault {
    /// No ambient host filesystem access.
    Deny,
}

/// A real directory covers its descendants; a file covers itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemRule {
    /// Denies override every overlapping allow.
    pub effect: RuleEffect,
    /// Absolute normalized path, without globs.
    pub path: String,
    /// Write implies read; deny-read also prevents writing.
    pub rights: Vec<FileRight>,
}

/// Rule effect independent of order or specificity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleEffect {
    /// Grant a declared right.
    Allow,
    /// Remove a right from all overlapping grants.
    Deny,
}

/// Coarse filesystem operations supported by bind mounts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileRight {
    /// Read contents and enumerate directories.
    Read,
    /// Read and mutate, including creating and removing directory entries.
    Write,
}

/// Mount-level noexec restrictions, not interpreter or alias prevention.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionPermissions {
    /// Paths whose visible mounts must reject direct execution.
    pub deny_paths: Vec<String>,
}

/// IP network mode for the entire process tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkPermissions {
    /// No target- or port-specific filtering is implied.
    pub mode: NetworkMode,
}

/// Supported network isolation levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    /// Block IPv4 and IPv6 communication.
    Off,
    /// Retain host networking with base process isolation.
    Full,
}

/// Checks a literal Linux path without consulting the host filesystem.
///
/// # Errors
/// Rejects relative paths, dot components, separators and glob expressions.
pub fn validate_os_path(path: &str) -> Result<(), ValidationError> {
    if !path.starts_with('/')
        || path.contains(['\0', '*', '?', '[', ']'])
        || (path != "/" && (path.ends_with('/') || path.contains("//")))
        || path.split('/').any(|part| matches!(part, "." | ".."))
    {
        return Err(ValidationError::new(
            "path",
            "expected a normalized absolute literal path",
        ));
    }
    Ok(())
}

impl Validate for OsPermissionSet {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.version != 1 {
            return Err(ValidationError::new(
                "version",
                "unsupported OS permission version",
            ));
        }
        if self.filesystem.rules.len() > 1024 || self.execution.deny_paths.len() > 1024 {
            return Err(ValidationError::new("permissions", "too many path rules"));
        }
        for rule in &self.filesystem.rules {
            validate_os_path(&rule.path)?;
            if rule.rights.is_empty()
                || rule.rights.len() > 2
                || (rule.rights.len() == 2 && rule.rights[0] == rule.rights[1])
            {
                return Err(ValidationError::new(
                    "rights",
                    "expected distinct nonempty read/write rights",
                ));
            }
        }
        for path in &self.execution.deny_paths {
            validate_os_path(path)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_ambiguous_path_syntax() {
        for path in [
            "relative",
            "/work/../etc",
            "/work/*",
            "/work/",
            "/a//b",
            "/a/./b",
        ] {
            assert!(validate_os_path(path).is_err(), "{path}");
        }
        for path in ["/", "/work", "/work/a b"] {
            assert!(validate_os_path(path).is_ok());
        }
    }

    #[test]
    fn rejects_unknown_fields_and_rights() {
        assert!(
            serde_json::from_str::<FilesystemRule>(
                r#"{"effect":"allow","path":"/w","rights":["execute"]}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<FilesystemRule>(
                r#"{"effect":"allow","path":"/w","rights":["read"],"scope":"subtree"}"#
            )
            .is_err()
        );
    }
}
