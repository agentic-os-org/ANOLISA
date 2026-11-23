//! Structural instantiation of credential token-file annotations in a packaged policy.

use std::collections::BTreeMap;
use std::str::FromStr as _;

use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::{Policy, PolicySet};
use serde_json::Value;

use crate::CedarError;
use crate::http::{CredentialRule, is_token_file};

const REAL: &str = "real_token_file";
const FAKE: &str = "fake_token_file";

/// Returns a copy of `source` whose credential rules read the given token files.
///
/// Every statement must be a credential rule. Only the two token-file annotation
/// values change: the result is rebuilt from the policy JSON structure, re-parsed and
/// compared statement by statement, so it cannot add rules, conditions or
/// annotations. Source comments are not retained.
///
/// # Errors
/// Rejects invalid or non-credential source, unsafe token paths, and any rebuilt
/// statement that differs from the original beyond the two path values.
pub fn rewrite_token_files(
    source: &CedarPolicySource,
    real_token_file: &str,
    fake_token_file: &str,
) -> Result<CedarPolicySource, CedarError> {
    let reject = |reason| CedarError::Unsupported {
        policy_id: "policy-set".into(),
        reason,
    };
    if !is_token_file(real_token_file)
        || !is_token_file(fake_token_file)
        || real_token_file == fake_token_file
    {
        return Err(reject(
            "token files must be distinct canonical absolute file paths without expansion",
        ));
    }
    let original_rules = crate::http::compile(source)?;
    let original = statements(&source.text)?;
    if original_rules.is_empty() || original_rules.len() != original.len() {
        return Err(reject("every statement must be a credential rule"));
    }
    let mut expected = BTreeMap::new();
    let mut text = Vec::with_capacity(original.len());
    for (order, (id, policy)) in original {
        let mut json = policy
            .to_json()
            .map_err(|_| reject("policy cannot be represented as JSON"))?;
        set_annotation(&mut json, REAL, real_token_file)?;
        set_annotation(&mut json, FAKE, fake_token_file)?;
        let rebuilt = Policy::from_json(Some(policy.id().clone()), json.clone())
            .map_err(|_| reject("rewritten policy JSON is invalid"))?;
        text.push(rebuilt.to_string());
        expected.insert(order, (id, json));
    }
    let rewritten = CedarPolicySource {
        schema: source.schema.clone(),
        text: text.join("\n\n") + "\n",
    };
    let actual = statements(&rewritten.text)?;
    if actual.len() != expected.len() {
        return Err(reject("rewritten policy changed the statement count"));
    }
    for ((order, (id, policy)), (expected_order, (expected_id, json))) in
        actual.into_iter().zip(expected)
    {
        let rebuilt = policy
            .to_json()
            .map_err(|_| reject("policy cannot be represented as JSON"))?;
        if order != expected_order || id != expected_id || rebuilt != json {
            return Err(reject("rewritten policy differs beyond token-file paths"));
        }
    }
    let rules = crate::http::compile(&rewritten)?;
    if !same_except_paths(&original_rules, &rules, real_token_file, fake_token_file) {
        return Err(reject(
            "rewritten credential rules differ beyond token-file paths",
        ));
    }
    Ok(rewritten)
}

/// Parses statements keyed by their source position, which the parser encodes in IDs.
fn statements(text: &str) -> Result<BTreeMap<usize, (String, Policy)>, CedarError> {
    let set = PolicySet::from_str(text).map_err(Box::new)?;
    let mut ordered = BTreeMap::new();
    for policy in set.policies() {
        let id = policy.id().to_string();
        let order = id
            .strip_prefix("policy")
            .and_then(|index| index.parse::<usize>().ok())
            .ok_or_else(|| CedarError::Unsupported {
                policy_id: id.clone(),
                reason: "statement identity does not follow source order",
            })?;
        let annotation = policy.annotation("id").unwrap_or(&id).to_owned();
        ordered.insert(order, (annotation, policy.clone()));
    }
    Ok(ordered)
}

fn set_annotation(json: &mut Value, name: &str, value: &str) -> Result<(), CedarError> {
    let slot = json
        .get_mut("annotations")
        .and_then(Value::as_object_mut)
        .and_then(|annotations| annotations.get_mut(name))
        .ok_or(CedarError::Unsupported {
            policy_id: "policy-set".into(),
            reason: "credential rule lacks a token-file annotation",
        })?;
    *slot = Value::String(value.to_owned());
    Ok(())
}

fn same_except_paths(
    original: &[CredentialRule],
    rewritten: &[CredentialRule],
    real_token_file: &str,
    fake_token_file: &str,
) -> bool {
    original.len() == rewritten.len()
        && original.iter().zip(rewritten).all(|(before, after)| {
            after.real_token_file == real_token_file
                && after.fake_token_file == fake_token_file
                && CredentialRule {
                    real_token_file: real_token_file.into(),
                    fake_token_file: fake_token_file.into(),
                    ..before.clone()
                } == *after
        })
}

#[cfg(test)]
mod tests {
    use asc_policy_types::cedar::CedarSchemaReference;
    use asc_policy_types::identifiers::{ResourceId, Revision};

    use super::*;

    const GITHUB: &str = include_str!("../../asc-gateway-plan/policies/github.cedar");
    const REAL_PATH: &str = "/run/agent-sec-core/inject-credential/abc/real";
    const FAKE_PATH: &str = "/run/agent-sec-core/inject-credential/abc/fake";

    fn source(text: &str) -> CedarPolicySource {
        CedarPolicySource {
            schema: CedarSchemaReference {
                schema_id: ResourceId::new(crate::schema::SCHEMA_ID).unwrap(),
                revision: Revision::new(crate::schema::HTTP_SCHEMA_REVISION).unwrap(),
            },
            text: text.into(),
        }
    }

    #[test]
    fn packaged_github_rules_change_only_token_paths() {
        let original = source(GITHUB);
        let rewritten = rewrite_token_files(&original, REAL_PATH, FAKE_PATH).unwrap();
        assert_eq!(
            original.text, GITHUB,
            "the packaged source is never modified"
        );
        crate::validate(&rewritten).unwrap();
        let before = crate::http::compile(&original).unwrap();
        let after = crate::http::compile(&rewritten).unwrap();
        assert_eq!(after.len(), 8);
        for (before, after) in before.iter().zip(&after) {
            assert_eq!(after.real_token_file, REAL_PATH);
            assert_eq!(after.fake_token_file, FAKE_PATH);
            assert_eq!(
                CredentialRule {
                    real_token_file: REAL_PATH.into(),
                    fake_token_file: FAKE_PATH.into(),
                    ..before.clone()
                },
                *after
            );
        }
        assert!(!rewritten.text.contains("/secure/github-"));
    }

    #[test]
    fn rewriting_is_deterministic() {
        let original = source(GITHUB);
        let first = rewrite_token_files(&original, REAL_PATH, FAKE_PATH).unwrap();
        let second = rewrite_token_files(&original, REAL_PATH, FAKE_PATH).unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn unsafe_or_identical_paths_are_rejected() {
        let original = source(GITHUB);
        for (real, fake) in [
            ("relative/real", FAKE_PATH),
            (REAL_PATH, "/run/../fake"),
            (REAL_PATH, REAL_PATH),
            ("/run/real*", FAKE_PATH),
        ] {
            assert!(rewrite_token_files(&original, real, fake).is_err());
        }
    }

    #[test]
    fn sources_with_non_credential_statements_are_rejected() {
        let file_policy =
            r#"forbid(principal, action == SecCore::Action::"file.delete", resource);"#;
        assert!(rewrite_token_files(&source(file_policy), REAL_PATH, FAKE_PATH).is_err());
        let mixed = format!("{GITHUB}\n{file_policy}\n");
        assert!(rewrite_token_files(&source(&mixed), REAL_PATH, FAKE_PATH).is_err());
    }
}
