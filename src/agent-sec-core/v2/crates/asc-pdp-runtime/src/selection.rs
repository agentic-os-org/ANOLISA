//! Selects the Cedar input for requests the launch Sandbox can issue.

use asc_policy_cedar::schema::uses_principal_targeting;
use asc_policy_cedar::selection::{SelectedStatement, Selection, ValidatedStatement};

/// Selects one statement from a policy admitted by the request-policy header check.
/// Legacy policy eligibility applies to the entire source. Only v3 uses projection.
#[must_use]
pub fn select(statement: &ValidatedStatement<'_>) -> Selection {
    let rule = statement.rule;
    let selected = if !uses_principal_targeting(statement.schema) {
        SelectedStatement::Whole
    } else if rule.may_match(&["file.write"], "SecCore::File") {
        // The launch request uses an ordinary write. Cedar still evaluates every
        // retained subject/path guard and any expression that cannot safely be split.
        statement.select_operations(&["write"])
    } else if rule.may_match(&["file.read", "file.exec"], "SecCore::File")
        || rule.may_match(&["process.spawn"], "SecCore::Process")
        || rule.may_match(&["network.use"], "SecCore::Network")
    {
        SelectedStatement::Whole
    } else {
        SelectedStatement::Unrelated
    };
    statement.selection(selected)
}
