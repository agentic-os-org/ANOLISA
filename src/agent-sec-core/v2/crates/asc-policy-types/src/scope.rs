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
    /// Caller-observed process id. PID reuse is handled during target resolution.
    Pid { pid: u32 },
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
            Self::Pid { .. } | Self::CgroupId { .. } => Ok(()),
        }
    }
}

/// Scope revision with unresolved selector intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PreparedScope {
    /// Stable scope identity.
    pub scope_id: ResourceId,
    /// Immutable revision.
    pub revision: Revision,
    /// Required unresolved caller intent; never inferred from the Scope identity.
    pub selector: ScopeSelector,
}

impl Validate for PreparedScope {
    fn validate(&self) -> Result<(), ValidationError> {
        self.selector.validate().map_err(|error| {
            ValidationError::new(format!("selector.{}", error.path), error.message)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
