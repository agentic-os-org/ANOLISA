//! Shared parsing and admission checks for the `SecCore` schema.

use asc_policy_types::Validate;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::{PolicySet, PrincipalConstraint, ValidationMode, ValidationWarning, Validator};
use cedar_policy_core::ast::{ExprKind, Var};

use crate::{
    CedarError,
    advice::Advice,
    schema::{RUNTIME_SCHEMA_REVISION, load_schema},
};

pub(super) fn parse_and_validate(
    source: &CedarPolicySource,
) -> Result<(PolicySet, Vec<ValidationWarning>), CedarError> {
    PolicySource::Cedar(source.clone()).validate()?;
    let schema = load_schema(&source.schema)?;
    let policies: PolicySet = source.text.parse().map_err(Box::new)?;
    let runtime = source.schema.revision.get() == RUNTIME_SCHEMA_REVISION;
    check_subset(&policies, runtime)?;

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
        if !runtime && references_principal(policy) {
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

fn check_subset(policies: &PolicySet, runtime: bool) -> Result<(), CedarError> {
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
        if !runtime && policy.principal_constraint() != PrincipalConstraint::Any {
            return Err(unsupported(
                policy.id().as_ref(),
                "principal constraints are assigned through Scope/Binding",
            ));
        }
        if runtime {
            for (name, value) in policy.annotations() {
                match name {
                    "id" if !value.is_empty() => {}
                    "advice" if policy.effect() == cedar_policy::Effect::Permit => {
                        Advice::parse(value).map_err(|_| {
                            unsupported(
                                policy.id().as_ref(),
                                "advice must follow the supported execution contract",
                            )
                        })?;
                    }
                    _ => {
                        return Err(unsupported(
                            policy.id().as_ref(),
                            "unsupported annotation or advice on forbid",
                        ));
                    }
                }
            }
        } else if policy.annotations().next().is_some() {
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
