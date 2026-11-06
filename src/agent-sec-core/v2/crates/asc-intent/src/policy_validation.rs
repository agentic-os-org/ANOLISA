//! Validates model-generated Cedar; it appends no rules and does not enforce policies.

use cedar_policy::{
    PolicySet, PrincipalConstraint, Schema, ValidationMode, ValidationWarning, Validator,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use std::str::FromStr as _;
use thiserror::Error;

/// Single embedded source of the shared policy type contract.
pub const CEDAR_SCHEMA: &str = include_str!("../../asc-policy-cedar/schema/seccore.cedarschema");
/// Policy text budget.
pub const MAX_POLICY_BYTES: usize = 256 * 1024;
pub(crate) const MAX_POLICIES: usize = 1024;

/// Validated Cedar text; validation is neither approval nor a completeness proof.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Artifact {
    /// Always cedar.
    pub language: String,
    /// Model text with no appended rules; may be empty.
    pub text: String,
    /// Digest of the current schema; not a signature.
    pub schema_sha256: String,
}

/// Cedar validation errors carry no model text or task paths.
#[derive(Debug, Error)]
pub enum PolicyError {
    /// The policy, the policy count or the final candidate exceeds its budget.
    #[error("candidate exceeds its size limit")]
    TooLarge,
    /// Candidate or Cedar AST encoding failed.
    #[error("candidate encoding failed")]
    Encoding,
    /// The official Cedar parser cannot parse the policy.
    #[error("generated candidate syntax is invalid")]
    Syntax,
    /// Official Strict type validation did not pass.
    #[error("generated candidate failed Cedar validation")]
    Validation,
    /// A path is not canonical and absolute, or uses an expression that cannot be checked.
    #[error("policy paths must use canonical absolute literals or directory subtree patterns")]
    InvalidPath,
    /// Unbound templates, self-declared identities or contract-less annotations cannot become a
    /// candidate.
    #[error("policy contains unsupported templates, identity references or annotations")]
    UnsupportedPolicy,
}

/// Parses the model policy, checks path expressions and runs Strict validation once.
///
/// Absolute paths from the task may point into any directory. Only expressions that can be
/// verified precisely are checked; task completeness is not inferred, OS objects are not bound
/// and advice or approvals are not discharged.
///
/// # Errors
/// Rejects invalid Cedar, non-canonical paths, unsupported policy features and oversized artifacts.
pub fn prepare_policy(model_text: &str) -> Result<Artifact, PolicyError> {
    let policies = parse(model_text)?;
    for policy in policies.policies() {
        if policy.annotations().any(|(name, _)| name != "id") {
            return Err(PolicyError::UnsupportedPolicy);
        }
        match policy.principal_constraint() {
            PrincipalConstraint::Any => {}
            PrincipalConstraint::Is(name) if name.to_string() == "SecCore::Subject" => {}
            _ => return Err(PolicyError::UnsupportedPolicy),
        }
        let est = policy.to_json().map_err(|_| PolicyError::Encoding)?;
        check_expression(&est["conditions"], 0)?;
        // File/Subject UIDs are neither task paths nor trusted instance identities.
        check_entity_references(&est)?;
    }
    let (schema, _) =
        Schema::from_cedarschema_str(CEDAR_SCHEMA).map_err(|_| PolicyError::Validation)?;
    let validation = Validator::new(schema).validate(&policies, ValidationMode::Strict);
    // Policy admission rejects an action that cannot apply to the resource type; the SDK reports
    // it as a warning, so `validation_passed` alone would emit a forbid that restricts nothing.
    let inapplicable_action = validation
        .validation_warnings()
        .any(|warning| matches!(warning, ValidationWarning::InvalidActionApplication(_)));
    if !validation.validation_passed() || inapplicable_action {
        return Err(PolicyError::Validation);
    }
    Ok(Artifact {
        language: "cedar".into(),
        text: model_text.to_owned(),
        schema_sha256: format!("sha256:{:x}", Sha256::digest(CEDAR_SCHEMA.as_bytes())),
    })
}

fn parse(text: &str) -> Result<PolicySet, PolicyError> {
    if text.len() > MAX_POLICY_BYTES {
        return Err(PolicyError::TooLarge);
    }
    let policies = PolicySet::from_str(text).map_err(|_| PolicyError::Syntax)?;
    if policies.templates().next().is_some() {
        return Err(PolicyError::UnsupportedPolicy);
    }
    if policies.policies().count() > MAX_POLICIES {
        return Err(PolicyError::TooLarge);
    }
    Ok(policies)
}

fn check_entity_references(value: &Value) -> Result<(), PolicyError> {
    match value {
        Value::Object(fields) => {
            if fields
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| matches!(kind, "SecCore::Subject" | "SecCore::File"))
            {
                return Err(PolicyError::UnsupportedPolicy);
            }
            for child in fields.values() {
                check_entity_references(child)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                check_entity_references(child)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn path_attribute(value: &Value) -> bool {
    value == &json!({".": {"left": {"Var": "resource"}, "attr": "path"}})
        || ["cwd", "program"].iter().any(|name| {
            value == &json!({".": {"left": {".": {"left": {"Var": "context"}, "attr": "input"}}, "attr": name}})
        })
}

fn check_expression(value: &Value, depth: usize) -> Result<(), PolicyError> {
    if depth > 128 {
        return Err(PolicyError::TooLarge);
    }
    // A path-carrying attribute must be read from its canonical receiver. A record projection
    // such as `{r: resource}.r.path` reaches the same string while hiding it from the checks
    // below, so any other receiver is rejected instead of being compared by shape.
    if let Some(attribute) = value["."]["attr"].as_str() {
        let canonical = match attribute {
            "path" => value["."]["left"] == json!({"Var": "resource"}),
            "cwd" | "program" => {
                value["."]["left"] == json!({".": {"left": {"Var": "context"}, "attr": "input"}})
            }
            _ => true,
        };
        if !canonical {
            return Err(PolicyError::InvalidPath);
        }
    }
    for op in ["==", "!="] {
        if let Some(binary) = value.get(op) {
            let (left, right) = (&binary["left"], &binary["right"]);
            let literal = if path_attribute(left) {
                Some(right)
            } else if path_attribute(right) {
                Some(left)
            } else {
                None
            };
            if let Some(literal) = literal {
                return if literal["Value"].as_str().is_some_and(absolute_path) {
                    Ok(())
                } else {
                    Err(PolicyError::InvalidPath)
                };
            }
        }
    }
    if let Some(like) = value.get("like")
        && path_attribute(&like["left"])
    {
        return check_pattern(&like["pattern"]);
    }
    // Policies are never rewritten by string replacement; an unchecked path read must fail rather
    // than be guessed.
    if path_attribute(value) {
        return Err(PolicyError::InvalidPath);
    }
    if value.get("Var").is_some_and(|var| var == "principal") {
        return Err(PolicyError::UnsupportedPolicy);
    }
    match value {
        Value::Object(fields) => {
            for child in fields.values() {
                check_expression(child, depth + 1)?;
            }
        }
        Value::Array(values) => {
            for child in values {
                check_expression(child, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn check_pattern(value: &Value) -> Result<(), PolicyError> {
    let elements = value.as_array().ok_or(PolicyError::InvalidPath)?;
    let mut prefix = String::new();
    let mut wildcard = false;
    for (index, element) in elements.iter().enumerate() {
        if element == "Wildcard" {
            if index + 1 != elements.len() || !prefix.ends_with('/') {
                return Err(PolicyError::InvalidPath);
            }
            wildcard = true;
        } else if let Some(literal) = element["Literal"].as_str() {
            prefix.push_str(literal);
        } else {
            return Err(PolicyError::InvalidPath);
        }
    }
    let path = if wildcard && prefix != "/" {
        prefix.strip_suffix('/').ok_or(PolicyError::InvalidPath)?
    } else {
        &prefix
    };
    if absolute_path(path) {
        Ok(())
    } else {
        Err(PolicyError::InvalidPath)
    }
}

/// Accepts canonical absolute paths only; relative and aliased forms must be rewritten by the
/// model or moved into unresolved.
fn absolute_path(path: &str) -> bool {
    path.len() <= 4096
        && (path == "/"
            || path.strip_prefix('/').is_some_and(|p| {
                !p.contains('\\')
                    && !p.chars().any(char::is_control)
                    && p.split('/')
                        .all(|part| !part.is_empty() && part != "." && part != "..")
            }))
}
