//! Shared parsing and admission checks for the `SecCore` schema.

use asc_policy_types::Validate;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::{PolicySet, PrincipalConstraint, ValidationMode, ValidationWarning, Validator};
use cedar_policy_core::ast::{ExprKind, Var};

use crate::{CedarError, schema::load_schema};

pub(super) fn parse_and_validate(
    source: &CedarPolicySource,
) -> Result<(PolicySet, Vec<ValidationWarning>), CedarError> {
    PolicySource::Cedar(source.clone()).validate()?;
    let schema = load_schema(&source.schema)?;
    let policies: PolicySet = source.text.parse().map_err(Box::new)?;
    check_subset(&policies)?;

    let validation = Validator::new(schema).validate(&policies, ValidationMode::Strict);
    // The SDK reports action applicability as a warning; admission rejects it.
    let rejected_warnings: Vec<_> = validation
        .validation_warnings()
        .filter(|warning| matches!(warning, ValidationWarning::InvalidActionApplication(_)))
        .cloned()
        .collect();
    if !validation.validation_passed() || !rejected_warnings.is_empty() {
        return Err(CedarError::Validation {
            errors: validation.validation_errors().cloned().collect(),
            warnings: rejected_warnings,
        });
    }

    for policy in policies.policies() {
        if references_principal(policy) {
            return Err(unsupported(
                policy.id().as_ref(),
                "principal conditions are assigned through Scope/Binding",
            ));
        }
    }
    Ok((
        policies,
        validation.validation_warnings().cloned().collect(),
    ))
}

fn check_subset(policies: &PolicySet) -> Result<(), CedarError> {
    if let Some(template) = policies.templates().next() {
        return Err(unsupported(
            template.id().as_ref(),
            "template linking is not supported",
        ));
    }
    if policies.policies().next().is_none() {
        return Err(unsupported(
            "policy-set",
            "at least one static policy is required",
        ));
    }
    for policy in policies.policies() {
        if policy.principal_constraint() != PrincipalConstraint::Any {
            return Err(unsupported(
                policy.id().as_ref(),
                "principal constraints are assigned through Scope/Binding",
            ));
        }
        if policy.annotations().next().is_some() {
            return Err(unsupported(
                policy.id().as_ref(),
                "annotations, including advice, are not supported",
            ));
        }
    }
    Ok(())
}

fn references_principal(policy: &cedar_policy::Policy) -> bool {
    // This SDK-internal AST access keeps admission independent of PST conversion.
    // cedar-policy-core is pinned to the SDK version for this boundary.
    let ast: &cedar_policy_core::ast::Policy = policy.as_ref();
    ast.non_scope_constraints().is_some_and(|expression| {
        expression
            .subexpressions()
            .any(|node| matches!(node.expr_kind(), ExprKind::Var(Var::Principal)))
    })
}

fn unsupported(policy_id: &str, reason: &'static str) -> CedarError {
    CedarError::Unsupported {
        policy_id: policy_id.to_owned(),
        reason,
    }
}
