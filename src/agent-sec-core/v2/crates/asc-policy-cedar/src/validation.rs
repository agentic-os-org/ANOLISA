//! Shared parsing and admission checks for the `SecCore` schema.

use asc_policy_types::Validate;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::{PolicySet, ValidationMode, ValidationWarning, Validator};

use crate::{CedarError, advice::Advice, schema::load_schema};

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
    }
    Ok(())
}

fn unsupported(policy_id: &str, reason: &'static str) -> CedarError {
    CedarError::Unsupported {
        policy_id: policy_id.to_owned(),
        reason,
    }
}
