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
    /// An equality literal ends with `/*`, which only a `like` pattern may express.
    #[error("equality compares a concrete path; write a directory subtree as like \"<dir>/*\"")]
    WildcardLiteral,
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

/// Lists the permits the launch-time authorization view cannot carry.
///
/// A task candidate is published once and read by the sandbox, the launch authorizer and the
/// runtime protector. The authorizer sees no request context, so its derived view drops every
/// permit that tests `context`; such a permit still projects into the sandbox but authorizes no
/// launch, which fails the command instead of narrowing it. Forbids are not listed: the view drops
/// them by design and the runtime protector reads them verbatim.
///
/// # Errors
/// Rejects text that `prepare_policy` would also reject as unparsable or oversized.
pub fn launch_view_gaps(text: &str) -> Result<Vec<String>, PolicyError> {
    let mut gaps = Vec::new();
    for (index, policy) in parse(text)?.policies().enumerate() {
        if policy.effect() != cedar_policy::Effect::Permit {
            continue;
        }
        let est = policy.to_json().map_err(|_| PolicyError::Encoding)?;
        let mut fields = Vec::new();
        if !context_fields(&est["conditions"], &mut fields) {
            continue;
        }
        fields.sort();
        fields.dedup();
        let action = est["action"]["entity"]["id"]
            .as_str()
            .unwrap_or("any action");
        let tested = if fields.is_empty() {
            "context".to_owned()
        } else {
            fields
                .iter()
                .map(|field| format!("context.input.{field}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let label = match policy.annotation("id") {
            Some(id) => format!("#{} @id({id:?})", index + 1),
            None => format!("#{}", index + 1),
        };
        gaps.push(format!("permit {label} ({action}) tests {tested}"));
    }
    Ok(gaps)
}

/// Lists the forbids the runtime protector cannot lower because they carry annotations.
///
/// The protector reads the published revision verbatim and refuses a forbid with any
/// annotation, `@id` included, so one annotated forbid fails the whole binding. Permits are not
/// listed: the protector skips them, and an `@id` there stays a description.
///
/// # Errors
/// Rejects text that `prepare_policy` would also reject as unparsable or oversized.
pub fn annotated_forbids(text: &str) -> Result<Vec<String>, PolicyError> {
    Ok(parse(text)?
        .policies()
        .enumerate()
        .filter(|(_, policy)| {
            policy.effect() == cedar_policy::Effect::Forbid && policy.annotations().next().is_some()
        })
        .map(|(index, policy)| {
            let keys: Vec<_> = policy
                .annotations()
                .map(|(key, _)| format!("@{key}"))
                .collect();
            format!("forbid #{} carries {}", index + 1, keys.join(", "))
        })
        .collect())
}

/// Lists the point grants that name a link instead of the object behind it.
///
/// The launch authorizer compares canonical paths and the sandbox refuses to grant a point under
/// a name whose canonical target differs, so a permit such as `resource.path == "/usr/bin/python3"`
/// matches nothing where that entry is a symlink, and a launch whose request covers it fails with
/// no authorized identity. Points the same action already grants as a subtree, including the
/// `==` half of a directory pair, are skipped: a directory pair such as `/lib` is portable and at
/// worst redundant. `resolve` maps a path to its canonical form, or `None` when it does not
/// exist; it reads metadata only.
///
/// # Errors
/// Rejects text that `prepare_policy` would also reject as unparsable or oversized.
pub fn point_aliases(
    text: &str,
    resolve: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<String>, PolicyError> {
    let mut permits = Vec::new();
    let mut subtrees: Vec<(String, String)> = Vec::new();
    for (index, policy) in parse(text)?.policies().enumerate() {
        if policy.effect() != cedar_policy::Effect::Permit {
            continue;
        }
        let est = policy.to_json().map_err(|_| PolicyError::Encoding)?;
        let action = est["action"]["entity"]["id"]
            .as_str()
            .unwrap_or("any action")
            .to_owned();
        let mut points = Vec::new();
        let mut trees = Vec::new();
        path_literals(&est["conditions"], &mut points, &mut trees);
        subtrees.extend(trees.into_iter().map(|tree| (action.clone(), tree)));
        permits.push((index, action, points));
    }
    let mut aliases = Vec::new();
    for (index, action, points) in permits {
        for point in points {
            let carried = subtrees.iter().any(|(granted, tree)| {
                granted == &action
                    && (tree == "/" || tree == &point || point.starts_with(&format!("{tree}/")))
            });
            if carried {
                continue;
            }
            if let Some(target) = resolve(&point).filter(|target| target != &point) {
                aliases.push(format!(
                    "permit #{} ({action}) grants {point}, which resolves to {target}",
                    index + 1
                ));
            }
        }
    }
    Ok(aliases)
}

/// Collects `resource.path == "..."` literals and the subtrees of `like "<dir>/*"` patterns.
fn path_literals(value: &Value, points: &mut Vec<String>, subtrees: &mut Vec<String>) {
    let path = json!({".": {"left": {"Var": "resource"}, "attr": "path"}});
    if let Some(binary) = value.get("==") {
        for (side, other) in [("left", "right"), ("right", "left")] {
            if binary[side] == path
                && let Some(literal) = binary[other]["Value"].as_str()
            {
                points.push(literal.to_owned());
            }
        }
    }
    if let Some(like) = value.get("like")
        && like["left"] == path
        && let Ok((tree, true)) = pattern_scope(&like["pattern"])
    {
        subtrees.push(tree);
    }
    match value {
        Value::Object(map) => {
            for child in map.values() {
                path_literals(child, points, subtrees);
            }
        }
        Value::Array(values) => {
            for child in values {
                path_literals(child, points, subtrees);
            }
        }
        _ => {}
    }
}

/// Returns whether `value` reads `context`, collecting the `context.input` fields it names.
fn context_fields(value: &Value, fields: &mut Vec<String>) -> bool {
    let input = json!({".": {"left": {"Var": "context"}, "attr": "input"}});
    if value["."]["left"] == input
        && let Some(field) = value["."]["attr"].as_str()
    {
        fields.push(field.to_owned());
        return true;
    }
    if value.get("Var").is_some_and(|var| var == "context") {
        return true;
    }
    let mut found = false;
    match value {
        Value::Object(map) => {
            for child in map.values() {
                found |= context_fields(child, fields);
            }
        }
        Value::Array(values) => {
            for child in values {
                found |= context_fields(child, fields);
            }
        }
        _ => {}
    }
    found
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
            "cwd" => {
                value["."]["left"] == json!({".": {"left": {"Var": "context"}, "attr": "input"}})
            }
            // `SecCore::Process.program` is a plain entity attribute holding a command name;
            // only the request context's `program` is an executable path.
            "program" => {
                value["."]["left"] == json!({"Var": "resource"})
                    || value["."]["left"]
                        == json!({".": {"left": {"Var": "context"}, "attr": "input"}})
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
                // A trailing `/*` belongs to a `like` pattern; as an equality literal it names no
                // concrete path and the projection cannot express it, so the model must retry.
                let Some(value) = literal["Value"].as_str() else {
                    return Err(PolicyError::InvalidPath);
                };
                if value.ends_with("/*") {
                    return Err(PolicyError::WildcardLiteral);
                }
                return if absolute_path(value) {
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
    pattern_scope(value).map(|_| ())
}

/// Decodes a `like` pattern into its canonical absolute prefix and whether a single trailing
/// wildcard covers the remaining subtree.
pub(crate) fn pattern_scope(value: &Value) -> Result<(String, bool), PolicyError> {
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
        Ok((path.to_owned(), wildcard))
    } else {
        Err(PolicyError::InvalidPath)
    }
}

/// Accepts canonical absolute paths only; relative and aliased forms must be rewritten by the
/// model or moved into unresolved.
pub(crate) fn absolute_path(path: &str) -> bool {
    path.len() <= 4096
        && (path == "/"
            || path.strip_prefix('/').is_some_and(|p| {
                !p.contains('\\')
                    && !p.chars().any(char::is_control)
                    && p.split('/')
                        .all(|part| !part.is_empty() && part != "." && part != "..")
            }))
}
