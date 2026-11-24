//! Automatic placement of file statements on discovered process trees.

use asc_policy_cedar::deployment::{DeploymentSelector, Placement, StatementTarget};
use asc_policy_types::policy::DeploymentBackend;

/// File statements establish their own process target, optionally restricted by UID.
pub const SELECTOR: DeploymentSelector = DeploymentSelector {
    backend: DeploymentBackend::ActPlane,
    select,
};

fn select(rule: &StatementTarget) -> Placement<'_> {
    if !rule.may_match(&["file.read", "file.write", "file.exec"], "SecCore::File") {
        return Placement::Ignore;
    }
    match &rule.subject {
        Ok(Some(subject)) => Placement::Bind(subject),
        Ok(None) => Placement::Unassigned(
            "automatic ActPlane deployment requires an exact principal.agentType".into(),
        ),
        Err(error) => Placement::Unassigned(error.to_string()),
    }
}
