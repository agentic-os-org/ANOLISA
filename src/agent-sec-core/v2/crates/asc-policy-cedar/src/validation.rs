//! Shared parsing and admission checks for the `SecCore` schema.

use asc_policy_types::Validate;
use asc_policy_types::authoring::PolicySource;
use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::{PolicySet, ValidationMode, ValidationWarning, Validator};

use crate::{
    CedarError,
    advice::Advice,
    http::{self, CredentialRule},
    schema::load_schema,
};

pub(super) fn parse_and_validate(
    source: &CedarPolicySource,
) -> Result<(PolicySet, Vec<ValidationWarning>, Vec<CredentialRule>), CedarError> {
    PolicySource::Cedar(source.clone()).validate()?;
    check_source_budget(&source.text)?;
    let schema = load_schema(&source.schema)?;
    let policies: PolicySet = source.text.parse().map_err(Box::new)?;
    check_subset(&policies)?;
    let rules = http::lower(&policies, source.schema.revision.get())?;

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
        rules,
    ))
}

// Bound recursive CST forms before SDK conversion, without counting flat binary operators.
fn check_source_budget(text: &str) -> Result<(), CedarError> {
    let reject = || {
        unsupported(
            "policy-set",
            "source exceeds 256 KiB, 64 nesting levels, prefix operators or conditionals, or 512 tokens per statement",
        )
    };
    if text.len() > 256 * 1024 {
        return Err(reject());
    }
    let bytes = text.as_bytes();
    let (mut index, mut depth, mut prefix, mut conditionals, mut tokens) = (0, 0_usize, 0, 0, 0);
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            index += 2;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            continue;
        }
        if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            index += 2;
            while index < bytes.len()
                && !(bytes[index] == b'*' && bytes.get(index + 1) == Some(&b'/'))
            {
                index += 1;
            }
            index = (index + 2).min(bytes.len());
            continue;
        }
        tokens += 1;
        if byte == b'"' {
            prefix = 0;
            index += 1;
            while index < bytes.len() {
                match bytes[index] {
                    b'\\' => index = (index + 2).min(bytes.len()),
                    b'"' => {
                        index += 1;
                        break;
                    }
                    _ => index += 1,
                }
            }
        } else if byte.is_ascii_alphanumeric() || byte == b'_' {
            prefix = 0;
            let start = index;
            index += 1;
            while index < bytes.len()
                && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
            {
                index += 1;
            }
            if &bytes[start..index] == b"if" {
                conditionals += 1;
            }
        } else {
            match byte {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => {
                    depth = depth.saturating_sub(1);
                    prefix = 0;
                }
                b';' => {
                    tokens = 0;
                    prefix = 0;
                    conditionals = 0;
                }
                b'!' | b'-' => prefix += 1,
                _ => {}
            }
            index += 1;
        }
        if depth > 64 || prefix > 64 || conditionals > 64 || tokens > 512 {
            return Err(reject());
        }
    }
    Ok(())
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
                name if http::is_annotation(name) => {}
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
