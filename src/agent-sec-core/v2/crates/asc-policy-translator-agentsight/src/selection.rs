//! Select the operations `ActPlane` can lower, then place them on discovered processes.

use asc_agentsight_protocol::FileProtectionTargetKind;
use asc_policy_cedar::{
    deployment::DeploymentSelector,
    file_protection,
    selection::{
        Direction, EnforcementDeviation, SelectedStatement, Selection, ValidatedStatement,
    },
};
use asc_policy_types::policy::DeploymentBackend;

/// File statements establish their own process target, optionally restricted by UID.
pub const SELECTOR: DeploymentSelector = DeploymentSelector {
    backend: DeploymentBackend::ActPlane,
    select,
    place: file_protection::placement,
    validate: Some(crate::validate_file_protection),
};

fn select(input: &ValidatedStatement<'_>) -> Selection {
    let selected = file_protection::select_operations(input);
    let fragment = match &selected {
        SelectedStatement::Fragment { cedar, .. } => Some(cedar.as_str()),
        SelectedStatement::Unrelated | SelectedStatement::Unsupported(_) => {
            return input.selection(selected);
        }
        _ => None,
    };
    let residual = match input.rule.deployment_rule(fragment) {
        Ok(rule) => rule,
        Err(error) => return input.selection(SelectedStatement::Unsupported(error.diagnostic())),
    };
    let syntax = cedar_policy::pst::PolicySet {
        policies: [(residual.id().clone(), residual)].into_iter().collect(),
        template_links: Vec::new(),
        templates: cedar_policy::pst::LinkedHashMap::new(),
    };
    let branches = match &selected {
        SelectedStatement::Fragment { branches, .. } => branches.clone(),
        _ => input.operation_branches(),
    };
    let mut affected = vec![false; branches.len()];
    let translated = asc_policy_cedar::file_protection::translate_with_origins(
        &syntax,
        crate::translate::cedar_target,
        branches.len(),
        |i, target| {
            affected[i] |= target.protections.delete
                || (target.kind == FileProtectionTargetKind::DirectoryTree
                    && target.protections.ordinary_write);
        },
    );
    // Admission considers the complete candidate. Dropping an unlowerable
    // statement here could make the rest of a previously rejected plan publish.
    if translated.is_err() {
        return input.selection(SelectedStatement::Undetermined);
    }
    let mut selection = input.selection(selected);
    for (branch, has_deviation) in branches.into_iter().zip(affected) {
        if !has_deviation {
            continue;
        }
        let (direction, code, condition, explanation) = match branch.operation.as_str() {
            "delete" => (
                Direction::Stricter,
                "ancestor_rename_guard",
                "Renaming an ancestor would move a protected path.",
                "ActPlane can block that rename to preserve deletion protection.",
            ),
            "write" => (
                Direction::Weaker,
                "shared_writable_mapping",
                "The selected directory tree is modified through a shared writable mapping.",
                "ActPlane cannot mediate every modification through that mapping.",
            ),
            _ => continue,
        };
        selection.deviations.push(EnforcementDeviation {
            branch,
            direction,
            code,
            condition,
            explanation,
        });
    }
    selection
}
