//! Target-independent Policy Scope contracts.

use serde::{Deserialize, Serialize};

use crate::error::{Validate, ValidationError};
use crate::identifiers::{ResourceId, Revision};

/// Caller intent used to locate a future trusted execution-domain identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ScopeSelector {
    /// Pins the first observed instance of this PID; never follows PID reuse.
    Pid { pid: u32 },
    /// Continuously selects existing and future processes.
    Process {
        /// Exactly one name or normalized executable path.
        #[serde(rename = "match")]
        matcher: ProcessMatcher,
    },
    /// Caller-observed cgroup id.
    CgroupId { cgroup_id: u64 },
}

impl Validate for ScopeSelector {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Pid { pid: 0 } => Err(ValidationError::new("pid", "must be positive")),
            Self::CgroupId { cgroup_id: 0 } => {
                Err(ValidationError::new("cgroupId", "must be positive"))
            }
            Self::Process { matcher } => matcher.validate(),
            Self::Pid { .. } | Self::CgroupId { .. } => Ok(()),
        }
    }
}

/// Exactly one process selection criterion; combinations and unknown fields reject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub enum ProcessMatcher {
    /// Exact kernel process name (`/proc/PID/comm`).
    Name {
        #[serde(rename = "processName")]
        process_name: String,
    },
    /// Exact normalized absolute executable path; no prefix or glob matching.
    Executable { executable: String },
}

impl Validate for ProcessMatcher {
    fn validate(&self) -> Result<(), ValidationError> {
        match self {
            Self::Name { process_name }
                if process_name.is_empty()
                    || process_name.len() > 15
                    || process_name.chars().any(char::is_control)
                    || process_name.contains('/') =>
            {
                Err(ValidationError::new(
                    "processName",
                    "must be a kernel process name of 1..15 bytes",
                ))
            }
            Self::Executable { executable }
                if !executable.starts_with('/')
                    || executable == "/"
                    || executable.contains("//")
                    || executable.ends_with('/')
                    || executable.chars().any(char::is_control)
                    || executable.split('/').any(|part| matches!(part, "." | "..")) =>
            {
                Err(ValidationError::new(
                    "executable",
                    "must be a normalized absolute executable path",
                ))
            }
            _ => Ok(()),
        }
    }
}

/// An exact Policy revision assigned to the selected subjects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PolicyReference {
    pub policy_id: ResourceId,
    pub policy_revision: Revision,
}

/// Assignment lifecycle; configuration remains immutable.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ScopeStatus {
    /// Discovery may admit new instances.
    #[default]
    Active,
    /// Admission is closed while owned deployments are cleaned up.
    Deleting,
}

/// Receipt for an idempotent Scope deletion request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScopeDeletion {
    /// Assignment whose admission is closed.
    pub scope_id: ResourceId,
    /// True only after discovery has stopped and all child responsibilities are gone.
    pub completed: bool,
}

/// Immutable assignment with complete policy snapshots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedScope {
    /// Stable assignment identity, never reused.
    pub scope_id: ResourceId,
    /// Authored selection criteria.
    pub selector: ScopeSelector,
    /// Server-resolved authored Policy revisions.
    pub policy_snapshots: Vec<crate::policy::PreparedPolicy>,
    /// Mutable lifecycle, independent of policy revisions.
    pub status: ScopeStatus,
}

impl Validate for PreparedScope {
    fn validate(&self) -> Result<(), ValidationError> {
        if self.policy_snapshots.is_empty() || self.policy_snapshots.len() > 32 {
            return Err(ValidationError::new(
                "policyTemplates",
                "requires 1..32 policies",
            ));
        }
        for (index, policy) in self.policy_snapshots.iter().enumerate() {
            policy.validate()?;
            if self.policy_snapshots[..index]
                .iter()
                .any(|p| p.policy_id == policy.policy_id)
            {
                return Err(ValidationError::new("policyTemplates", "duplicate policy"));
            }
        }
        self.selector.validate()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_matchers_reject_combinations_and_noncanonical_paths() {
        for input in [
            "{}",
            r#"{"processName":"agent","executable":"/bin/agent"}"#,
            r#"{"processName":"agent","extra":true}"#,
        ] {
            assert!(serde_json::from_str::<ProcessMatcher>(input).is_err());
        }
        for path in ["relative", "/a/../b", "/a/./b", "/a//b", "/a/", "/"] {
            assert!(
                ProcessMatcher::Executable {
                    executable: path.to_owned()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            ProcessMatcher::Executable {
                executable: "/opt/agent".to_owned()
            }
            .validate()
            .is_ok()
        );
        assert!(
            ProcessMatcher::Name {
                process_name: String::new()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn scope_validation_rejects_unusable_selectors() {
        assert_eq!(
            ScopeSelector::Pid { pid: 0 }.validate().unwrap_err().path,
            "pid"
        );

        assert_eq!(
            ScopeSelector::CgroupId { cgroup_id: 0 }
                .validate()
                .unwrap_err()
                .path,
            "cgroupId"
        );
    }
}
