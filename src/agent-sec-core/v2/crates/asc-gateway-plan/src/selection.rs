//! Automatic placement of credential grants and HTTP rules which may constrain them.

use asc_policy_cedar::deployment::{DeploymentSelector, Placement, StatementTarget};
use asc_policy_cedar::selection::{SelectedStatement, Selection, ValidatedStatement};
use asc_policy_types::policy::DeploymentBackend;

/// HTTP credential scopes are created only by grants with their own verified target.
pub const SELECTOR: DeploymentSelector = DeploymentSelector {
    backend: DeploymentBackend::Gateway,
    select,
    place,
};

fn select(input: &ValidatedStatement<'_>) -> Selection {
    let selected = match place(input.rule) {
        Placement::Ignore => SelectedStatement::Unrelated,
        Placement::Unassigned(reason) => SelectedStatement::Unsupported(reason),
        Placement::Bind(_) => SelectedStatement::Whole,
        // The full restriction is required for Scope admission. Selection alone
        // cannot promise that the resulting combination can be installed.
        Placement::Include(_) => SelectedStatement::Undetermined,
    };
    input.selection(selected)
}

fn place(rule: &StatementTarget) -> Placement<'_> {
    if !rule.may_match(&["network.send"], "SecCore::HttpEndpoint") {
        return Placement::Ignore;
    }
    if rule.credential_rule.is_none() {
        // A restriction with an unrepresentable subject must still reach the compiler,
        // which rejects the candidate if the restriction cannot be enforced.
        return Placement::Include(rule.subject.as_ref().ok().and_then(Option::as_ref));
    }
    match &rule.subject {
        Ok(Some(subject)) if subject.owner_uid.is_some_and(|uid| uid != 0) => Placement::Bind(subject),
        Ok(_) => Placement::Unassigned("automatic credential grants require an exact agentType and a non-root ownerUid on this statement".into()),
        Err(error) => Placement::Unassigned(error.to_string()),
    }
}
