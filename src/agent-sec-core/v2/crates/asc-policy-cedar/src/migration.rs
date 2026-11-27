//! Structural conversions for Cedar source saved before statement-based placement.
use asc_policy_types::cedar::CedarPolicySource;
use cedar_policy::{Policy, PrincipalConstraint};
use serde_json::{Value, json};

use crate::{CedarError, deployment::StatementTarget};

/// Convert a saved source while preserving request eligibility and parser IDs.
/// Pinned process snapshots only need the subject spelling conversion.
///
/// # Errors
/// Rejects unrepresentable source without changing the database.
pub fn upgrade_text(
    source: &CedarPolicySource,
    old_revision: u32,
    current: bool,
) -> Result<Option<String>, CedarError> {
    crate::validation::check_source_budget(&source.text)?;
    let original = crate::rewrite::statements(&source.text)?;
    let mut changed = false;
    let mut ordered = Vec::with_capacity(original.len());
    for (_, (_, policy)) in original {
        let reject = |reason| CedarError::Unsupported {
            policy_id: policy.id().to_string(),
            reason,
        };
        let mut est = policy
            .to_json()
            .map_err(|_| reject("cannot read saved Cedar statement"))?;
        let renamed = rename_subject_attribute(&mut est["conditions"]);
        let migrated = if renamed {
            Policy::from_json(Some(policy.id().clone()), est.clone())
                .map_err(|_| reject("invalid migrated subject attribute"))?
        } else {
            policy.clone()
        };
        let mut modified = renamed;
        if current {
            let rule = StatementTarget::from_policy(&migrated)?;
            if old_revision == 3
                && rule.bare_principal()
                && rule.may_match(&["file.read", "file.write", "file.exec"], "SecCore::File")
            {
                // The previous revision 3 evaluated bare file rules for every Sandbox.
                est["principal"] = json!({"op":"is","entity_type":"SecCore::Subject"});
                modified = true;
            } else if old_revision < 3
                && matches!(policy.principal_constraint(), PrincipalConstraint::Any)
                && rule.request_eligible()
            {
                // Old bare-header policies never participated in request authorization.
                // A process always has a UID, while Sandbox requests omit this attribute.
                est["conditions"].as_array_mut().ok_or_else(|| reject("missing Cedar conditions"))?.insert(0,
                    json!({"kind":"when","body":{"has":{"left":{"Var":"principal"},"attr":"ownerUid"}}}));
                modified = true;
            }
        }
        changed |= modified;
        let text = if modified {
            Policy::from_json(Some(policy.id().clone()), est)
                .map_err(|_| reject("invalid migrated Cedar statement"))?
                .to_string()
        } else {
            policy.to_string()
        };
        ordered.push(text);
    }
    Ok(changed.then(|| ordered.join("\n")))
}

// Only visit Cedar expression operators. Literal records may themselves contain
// keys named "has", "Var" or ".", and are data even when shaped like an expression.
fn rename_subject_attribute(value: &mut Value) -> bool {
    match value {
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                changed |= rename_subject_attribute(item);
            }
            changed
        }
        Value::Object(fields) if !fields.contains_key("Value") => {
            let mut changed = false;
            for (key, node) in fields {
                if matches!(key.as_str(), "." | "has")
                    && node.get("left") == Some(&json!({"Var":"principal"}))
                {
                    match node.get_mut("attr") {
                        Some(attr) if attr.as_str() == Some("agent_type") => {
                            *attr = json!("agentType");
                            changed = true;
                        }
                        Some(Value::Array(path))
                            if path.first().is_some_and(|attr| attr == "agent_type") =>
                        {
                            path[0] = json!("agentType");
                            changed = true;
                        }
                        _ => {}
                    }
                }
                changed |= rename_subject_attribute(node);
            }
            changed
        }
        _ => false,
    }
}
