//! Shared tool callback payloads and bounded routes for explicit SDK factories.

use crate::{
    adapter::{HookEvent, NativeContext},
    Result,
};
use aw_provider::admission::AdmittedStep;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) fn routes(
    document: &Value,
    steps: &[AdmittedStep],
) -> Result<(BTreeMap<String, HookEvent>, Vec<Value>)> {
    let mut events = BTreeMap::new();
    let mut hooks = Vec::new();
    for event in ["tool.before", "tool.after"] {
        let selected: Vec<_> = steps.iter().filter(|step| step.event == event).collect();
        if selected.is_empty() {
            continue;
        }
        let budget = document["spec"]["events"][event]
            .get("budget_ms")
            .unwrap_or(&document["spec"]["execution"]["default_event_budget_ms"])
            .as_f64()
            .ok_or("invalid SDK event budget")? as u64;
        if !(1..=55000).contains(&budget) {
            return Err("SDK hook budgets must be 1..55000 ms".into());
        }
        for step in &selected {
            hooks.push(json!({"event": event, "step": step.step_id, "on_error": step.on_error, "budget_ms": budget, "replace_result": step.execution.replaces_result()}));
        }
        events.insert(
            event.into(),
            HookEvent {
                budget_ms: budget,
                replacement_step: selected
                    .iter()
                    .find(|step| step.execution.replaces_result())
                    .map(|step| step.step_id.clone()),
                steps: selected
                    .into_iter()
                    .map(|step| (step.step_id.clone(), step.on_error.clone()))
                    .collect(),
            },
        );
    }
    Ok((events, hooks))
}

// SDK dialects share tool payload fields and preserve native session/call identities.
pub(super) fn normalize_tools(
    binding: &NativeContext,
    event: &str,
    native: &Value,
    adapter: &str,
) -> Result<Value> {
    let name = native["hook_event_name"].as_str();
    if !matches!(
        (event, name),
        ("tool.before", Some("PreToolUse"))
            | ("tool.after", Some("PostToolUse" | "PostToolUseFailure"))
    ) {
        return Err("SDK event differs from its callback route".into());
    }
    let id = |value: &Value| -> Result<String> {
        value
            .as_str()
            .filter(|s| !s.trim().is_empty() && s.len() <= 512)
            .map(str::to_owned)
            .ok_or_else(|| "SDK requires bounded native identities".into())
    };
    let session = id(&native["session_id"])?;
    let call = id(&native["tool_use_id"])?;
    let tool = id(&native["tool_name"])?;
    if native["cwd"].as_str() != binding.cwd.to_str() || !native["tool_input"].is_object() {
        return Err("SDK callback requires bound cwd and object tool_input".into());
    }
    let result = match name {
        Some("PostToolUse") => native
            .get("tool_response")
            .ok_or("SDK success callback requires tool_response")?
            .clone(),
        Some("PostToolUseFailure") => {
            if !native["error"].is_string() {
                return Err("SDK failure callback requires error".into());
            }
            json!({"is_error": true, "error": native["error"], "is_interrupt": native["is_interrupt"]})
        }
        _ => Value::Null,
    };
    let standard = match tool.as_str() {
        "Bash" => "bash",
        "Read" => "file_read",
        "Write" | "Edit" => "file_write",
        _ => &tool,
    };
    Ok(
        json!({"name": event, "agent": {"adapter": adapter, "binding_id": binding.target, "instance_id": binding.instance_id}, "session_id": session,
            "tool": {"name": standard, "native_name": tool, "call_id": call, "input": native["tool_input"], "result": result}, "native": native}),
    )
}

pub(super) fn invocation_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, byte)| match i {
            8 | 13 | 18 | 23 => byte == b'-',
            14 => byte == b'4',
            19 => matches!(byte, b'8' | b'9' | b'a' | b'b'),
            _ => byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'),
        })
}
