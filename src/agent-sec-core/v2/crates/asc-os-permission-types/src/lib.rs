//! OS permissions shared by authorization and enforcement.

#![forbid(unsafe_code)]

pub use asc_foundation_types::{Validate, ValidationError};
use serde::{Deserialize, Serialize};

/// Versioned permissions shared by requests and execution backends.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OsPermissionSet {
    /// Wire version; currently one, including optional subtree exclusions.
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
    /// Subtrees outside this deny's scope; exclusions never grant access.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_paths: Vec<String>,
}

impl FilesystemRule {
    /// Matches the rule's range after subtracting its explicit exclusions.
    pub fn covers_path(&self, path: &str, directory: bool) -> bool {
        (path == self.path || directory && path_contains(&self.path, path))
            && !self
                .excluded_paths
                .iter()
                .any(|excluded| path_contains(excluded, path))
    }
}

/// Tests literal containment at a path-component boundary.
pub fn path_contains(parent: &str, child: &str) -> bool {
    child == parent
        || parent == "/"
        || child
            .strip_prefix(parent)
            .is_some_and(|tail| tail.starts_with('/'))
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
        let boundary_count = self
            .filesystem
            .rules
            .iter()
            .map(|rule| 1 + rule.excluded_paths.len())
            .sum::<usize>();
        if boundary_count > 1024 || self.execution.deny_paths.len() > 1024 {
            return Err(ValidationError::new("permissions", "too many path rules"));
        }
        for rule in &self.filesystem.rules {
            validate_os_path(&rule.path)?;
            if !rule.excluded_paths.is_empty() && rule.effect != RuleEffect::Deny {
                return Err(ValidationError::new(
                    "excluded_paths",
                    "subtree exclusions require a deny rule",
                ));
            }
            if rule.excluded_paths.len() > 1024 {
                return Err(ValidationError::new(
                    "excluded_paths",
                    "too many subtree exclusions",
                ));
            }
            for (index, excluded) in rule.excluded_paths.iter().enumerate() {
                validate_os_path(excluded)?;
                if excluded == &rule.path
                    || !path_contains(&rule.path, excluded)
                    || rule.excluded_paths[..index].iter().any(|other| {
                        path_contains(other, excluded) || path_contains(excluded, other)
                    })
                {
                    return Err(ValidationError::new(
                        "excluded_paths",
                        "expected distinct nonoverlapping strict descendant subtrees",
                    ));
                }
            }
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

    #[test]
    fn subtree_exclusions_keep_v1_and_only_narrow_denies() {
        let value = serde_json::json!({
            "version": 1, "filesystem": {"default": "deny", "rules": [
                {"effect":"deny", "path":"/a", "rights":["read"], "excluded_paths":["/a/b"]}
            ]}, "execution":{"deny_paths":[]}, "network":{"mode":"off"}
        });
        let mut permissions: OsPermissionSet = serde_json::from_value(value).unwrap();
        permissions.validate().unwrap();
        let rule = &permissions.filesystem.rules[0];
        for path in ["/a", "/a/other", "/a/b-other"] {
            assert!(rule.covers_path(path, true), "{path}");
        }
        for path in ["/a/b", "/a/b/file", "/abc"] {
            assert!(!rule.covers_path(path, true), "{path}");
        }
        for version in [0, 2, u32::MAX] {
            permissions.version = version;
            assert!(permissions.validate().is_err());
        }
        permissions.version = 1;
        permissions.filesystem.rules[0].effect = RuleEffect::Allow;
        assert!(permissions.validate().is_err());
        permissions.filesystem.rules[0].effect = RuleEffect::Deny;
        for excluded in [
            vec!["/a"],
            vec!["/abc"],
            vec!["/a/b", "/a/b/c"],
            vec!["/a/b", "/a/b"],
        ] {
            permissions.filesystem.rules[0].excluded_paths =
                excluded.into_iter().map(str::to_owned).collect();
            assert!(permissions.validate().is_err());
        }
        permissions.filesystem.rules[0].excluded_paths.clear();
        permissions.validate().unwrap();
        assert!(
            !serde_json::to_string(&permissions)
                .unwrap()
                .contains("excluded_paths")
        );
    }
}
