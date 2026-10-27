//! Stable bridge plans keep native scheduling and lifecycle interpretation in Adapters.

use crate::{adapter, HookEvent, HookOutput, NativeContext, NativeExit, Result};
use aw_provider::admission::{AdapterCapabilities, AdmittedStep};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[cfg(test)]
mod tests;

/// Reserved callback route; cannot collide with a configured step identifier.
pub const DISPATCH: &str = "$aw.dispatch";

/// One dynamically admitted callback, retaining its own error action.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// Configured step ID.
    pub id: String,
    /// Existing report/block failure policy.
    pub on_error: String,
}

/// Adapter-selected scheduling for one native event under an immutable policy.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// Shared event budget, independent of the native bridge's transport ceiling.
    pub budget_ms: u64,
    /// Groups execute in order; steps within a group execute concurrently.
    pub groups: Vec<Vec<Step>>,
    /// Whether native normalization must include the result projection.
    pub replacement: bool,
    /// Retain optional pure observation even when the policy has no steps.
    pub observe: bool,
}

/// Check the initially pinned version and reusable native entrypoint.
pub fn supported(capabilities: &AdapterCapabilities) -> Result<()> {
    match (
        capabilities.adapter.as_str(),
        capabilities.version.as_str(),
        capabilities.entrypoint.as_str(),
    ) {
        ("qoder", "1.1.73", "tui") | ("qwenpaw", "2.2.2.post1", "app")
        | ("qoder-sdk", "1.0.56", "sdk-worker") => Ok(()),
        _ => Err("runtime policy updates require Qoder 1.1.73 TUI, QwenPaw 2.2.2.post1 App or Qoder SDK 1.0.56".into()),
    }
}

/// Check the ordinary resident adapters with immutable task policy snapshots.
pub fn resident_supported(capabilities: &AdapterCapabilities) -> Result<()> {
    if !matches!(capabilities.adapter.as_str(), "qoder-sdk" | "qwenpaw") {
        return Err("resident policy updates require Qoder SDK or QwenPaw".into());
    }
    supported(capabilities)
}

/// Validate a replacement with the ordinary factory's native routing contract.
pub fn validate_resident(
    document: &Value,
    target: &str,
    capabilities: &AdapterCapabilities,
) -> Result<()> {
    resident_supported(capabilities)?;
    let config = aw_config::Validator::new()
        .map_err(|error| error.to_string())?
        .parse(&serde_json::to_vec(document)?)
        .map_err(|error| error.to_string())?;
    let steps = aw_provider::admission::preflight(&config, target, capabilities)
        .map_err(|error| error.to_string())?;
    scope_policy(document, target, &steps, capabilities)?;
    Ok(())
}

/// Project an immutable task's routes without exposing Provider configuration.
pub fn scope_policy(
    document: &Value,
    target: &str,
    steps: &[AdmittedStep],
    capabilities: &AdapterCapabilities,
) -> Result<Value> {
    resident_supported(capabilities)?;
    if document["spec"]["agents"][target]["adapter"] != capabilities.adapter {
        return Err("resident policy adapter differs from its target".into());
    }
    if adapter::observation_enabled(document, target) {
        return Err("resident policy scopes do not support native evidence capture".into());
    }
    let (events, hooks) = match capabilities.adapter.as_str() {
        "qoder-sdk" => crate::adapters::sdk_tools::routes(document, steps)?,
        "qwenpaw" => crate::adapters::qwenpaw::event_routes(document, steps, false)?,
        _ => return Err("unsupported resident policy adapter".into()),
    };
    Ok(json!({"events": events, "hooks": hooks}))
}

/// Validate native scheduling and budget constraints before Provider preparation.
pub fn validate(document: &Value, target: &str, capabilities: &AdapterCapabilities) -> Result<()> {
    supported(capabilities)?;
    let bytes = serde_json::to_vec(document)?;
    let config = aw_config::Validator::new()
        .map_err(|e| e.to_string())?
        .parse(&bytes)
        .map_err(|e| e.to_string())?;
    let steps = aw_provider::admission::preflight(&config, target, capabilities)
        .map_err(|e| e.to_string())?;
    for event in ["tool.before", "tool.after"] {
        plan(document, target, &steps, event)?;
    }
    Ok(())
}

/// Build callback order using the same native rules as launch-time registration.
pub fn plan(document: &Value, target: &str, steps: &[AdmittedStep], event: &str) -> Result<Plan> {
    if !matches!(event, "tool.before" | "tool.after") {
        return Err("unsupported native event".into());
    }
    let adapter = document["spec"]["agents"][target]["adapter"]
        .as_str()
        .ok_or("missing adapter")?;
    let selected = steps
        .iter()
        .filter(|step| step.event == event)
        .collect::<Vec<_>>();
    let replacement = selected.iter().any(|step| step.execution.replaces_result());
    let mut ordered = selected
        .iter()
        .map(|step| Step {
            id: step.step_id.clone(),
            on_error: step.on_error.clone(),
        })
        .collect::<Vec<_>>();
    let sequential = match adapter {
        "qoder" => document["spec"]["agents"][target]["qoder"]["sequential"] == true,
        "qoder-sdk" => true,
        "qwenpaw" => {
            if event == "tool.after" && !replacement {
                ordered.reverse();
            }
            true
        }
        _ => return Err("unsupported runtime policy adapter".into()),
    };
    let budget = document["spec"]["events"][event]
        .get("budget_ms")
        .unwrap_or(&document["spec"]["execution"]["default_event_budget_ms"])
        .as_f64()
        .ok_or("invalid event budget")? as u64;
    if !(1..=55000).contains(&budget) || (replacement && !sequential) {
        return Err("runtime policy requires 1..55000 ms budgets and ordered replacement".into());
    }
    Ok(Plan {
        budget_ms: budget,
        groups: if sequential {
            ordered.into_iter().map(|step| vec![step]).collect()
        } else if ordered.is_empty() {
            vec![]
        } else {
            vec![ordered]
        },
        replacement,
        observe: adapter::observation_enabled(document, target),
    })
}

/// Register both stable events even if the initial policy is empty.
pub fn routes() -> BTreeMap<String, HookEvent> {
    ["tool.before", "tool.after"]
        .into_iter()
        .map(|event| {
            (
                event.into(),
                HookEvent {
                    budget_ms: 55000,
                    replacement_step: None,
                    steps: BTreeMap::from([(
                        DISPATCH.into(),
                        if event == "tool.before" {
                            "block"
                        } else {
                            "report"
                        }
                        .into(),
                    )]),
                },
            )
        })
        .collect()
}

/// Normalize only trustworthy Qoder lifecycle boundaries; Stop can request continuation.
///
/// The idle-prompt notification confirms return to native input. Stop, StopFailure
/// and model waits do not by themselves release request admission.
pub fn qoder_lifecycle(context: &NativeContext, native: &Value) -> Result<(&'static str, String)> {
    if native["cwd"].as_str() != context.cwd.to_str() {
        return Err("lifecycle cwd mismatch".into());
    }
    let session = native["session_id"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 512)
        .ok_or("missing native session")?;
    let phase = match native["hook_event_name"].as_str() {
        Some("SessionStart") => return Ok(("ready", String::new())),
        Some("UserPromptSubmit") => "request_start",
        Some("SessionEnd") => "request_end",
        Some("Notification") if native["notification_type"] == "idle_prompt" => "request_end",
        Some("PostToolUseFailure" | "PermissionDenied") => {
            let call = native["tool_use_id"]
                .as_str()
                .filter(|s| !s.is_empty())
                .ok_or("missing tool identity")?;
            return Ok(("tool_end", tool_key(session, call)?));
        }
        _ => return Err("unsupported Qoder lifecycle boundary".into()),
    };
    Ok((phase, session.into()))
}

/// Unambiguous bounded native tool identity, shared by before and after callbacks.
pub fn tool_key(session: &str, call: &str) -> Result<String> {
    use sha2::{Digest, Sha256};
    Ok(format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&json!([session, call]))?)
    ))
}

/// Combine dynamic Qoder replies using native veto precedence and JSON envelopes.
pub fn combine(adapter: &str, outputs: Vec<HookOutput>) -> Result<HookOutput> {
    let mut result = HookOutput {
        exit: NativeExit::Code(0),
        stdout: Vec::new(),
        stderr: Vec::new(),
    };
    let mut value = json!({});
    for output in outputs {
        result.stderr.extend(output.stderr);
        if matches!(output.exit, NativeExit::Code(2)) {
            if adapter == "qoder-sdk" && !matches!(result.exit, NativeExit::Code(2)) {
                // SDK denial reasons are JSON strings, not mergeable Hook objects.
                result.stdout = output.stdout.clone();
            }
            result.exit = NativeExit::Code(2);
        } else if !matches!(output.exit, NativeExit::Code(0))
            && matches!(result.exit, NativeExit::Code(0))
        {
            result.exit = output.exit;
        }
        if !output.stdout.iter().all(u8::is_ascii_whitespace) {
            // Tool hooks ignore plain-text stdout in both native dialects.
            if let Ok(next @ Value::Object(_)) = serde_json::from_slice::<Value>(&output.stdout) {
                merge(adapter, &mut value, next);
            }
        }
    }
    if matches!(result.exit, NativeExit::Code(2)) {
        if adapter == "qoder-sdk" {
            return Ok(result);
        }
        value = json!({});
    }
    if adapter == "qoder" || value != json!({}) {
        result.stdout = serde_json::to_vec(&value)?;
    }
    Ok(result)
}

/// Adopt each step's native exit and failure action before combining responses.
pub fn adopt_step(
    adapter: &str,
    event: &str,
    on_error: &str,
    mut output: HookOutput,
) -> Result<HookOutput> {
    let succeeded = matches!(output.exit, NativeExit::Code(0));
    let mut value = serde_json::from_slice::<Value>(&output.stdout).ok();
    if adapter == "qoder" && succeeded && event == "tool.before" {
        if let Some(value) = value.as_mut() {
            if let Some(decision) = qoder_decision(value).map(str::to_owned) {
                // Normalize each reply before merging: a nested permission
                // overrides its own legacy decision, not another hook's veto.
                let fields = value.as_object_mut().ok_or("invalid Qoder hook response")?;
                fields.remove("decision");
                let specific = fields
                    .entry("hookSpecificOutput")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .ok_or("invalid Qoder hookSpecificOutput")?;
                specific.insert("hookEventName".into(), json!("PreToolUse"));
                specific.insert("permissionDecision".into(), json!(decision));
                output.stdout = serde_json::to_vec(value)?;
            }
        }
    }
    if adapter == "qwenpaw"
        && value.as_ref().is_some_and(|value| {
            matches!(value["action"].as_str(), Some("ask" | "approve"))
                || value["decision"] == "ask"
        })
    {
        return Err("unsupported native approval".into());
    }
    if !matches!(output.exit, NativeExit::Code(0))
        && !(event == "tool.before" && matches!(output.exit, NativeExit::Code(2)))
    {
        // Qoder logs nonblocking exits. QwenPaw applies each wrapper's failure
        // policy; a report-only failure must not hide another step's veto/result.
        output.exit = NativeExit::Code(
            if matches!(adapter, "qwenpaw" | "qoder-sdk")
                && event == "tool.before"
                && on_error == "block"
            {
                2
            } else {
                0
            },
        );
        output.stdout.clear();
    }
    if adapter == "qoder"
        && succeeded
        && event == "tool.before"
        && value
            .as_ref()
            .is_some_and(|value| qoder_decision(value) == Some("deny"))
    {
        let reason = value.as_ref().and_then(|value| {
            value["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .or(value["reason"].as_str())
        });
        if let Some(reason) = reason {
            output.stderr.extend_from_slice(reason.as_bytes());
        }
        output.exit = NativeExit::Code(2);
        output.stdout.clear();
    }
    Ok(output)
}

/// Recognize Qoder's exit-code and JSON vetoes when completing tool admission.
pub fn denied(output: &HookOutput) -> bool {
    matches!(output.exit, NativeExit::Code(2))
        || serde_json::from_slice::<Value>(&output.stdout)
            .is_ok_and(|value| qoder_decision(&value) == Some("deny") || value["continue"] == false)
}

fn qoder_decision(value: &Value) -> Option<&str> {
    match value["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .filter(|decision| matches!(*decision, "allow" | "deny" | "ask"))
        .or(value["decision"].as_str())?
    {
        "approve" => Some("allow"),
        "block" => Some("deny"),
        decision @ ("allow" | "deny" | "ask") => Some(decision),
        _ => None,
    }
}

fn merge(adapter: &str, value: &mut Value, next: Value) {
    match (value, next) {
        (Value::Object(value), Value::Object(next)) => {
            for (key, next) in next {
                if let Some(previous) = value.get_mut(&key) {
                    if matches!(key.as_str(), "decision" | "permissionDecision")
                        && (*previous == "deny" || next == "deny")
                    {
                        *previous = json!("deny");
                    } else if adapter == "qoder"
                        && matches!(key.as_str(), "decision" | "permissionDecision")
                        && (*previous == "ask" || next == "ask")
                    {
                        *previous = json!("ask");
                    } else if adapter == "qoder" && key == "additionalContext" {
                        if let (Value::String(first), Value::String(second)) = (&*previous, &next) {
                            *previous = json!(format!("{first}\n{second}"));
                        } else {
                            *previous = next;
                        }
                    } else if key == "continue" && (*previous == false || next == false) {
                        *previous = json!(false);
                    } else if matches!(
                        key.as_str(),
                        "updatedToolOutput" | "updatedMCPToolOutput" | "aw_replacement"
                    ) {
                        *previous = next;
                    } else {
                        merge(adapter, previous, next);
                    }
                } else {
                    value.insert(key, next);
                }
            }
        }
        (previous, next) => *previous = next,
    }
}
