//! Corroborate normalized IDs with original bytes from the admitted same-user Adapter.

use aw_contracts::canonical;
use serde_json::{json, Value};

fn identifier(value: &Value) -> Option<&str> {
    value.as_str().filter(|text| {
        !text.trim().is_empty() && text.len() <= 512 && !text.chars().any(char::is_control)
    })
}

pub(in crate::runtime) fn association(adapter: &str, event: &Value, input: Option<&[u8]>) -> Value {
    let session = identifier(&event["session_id"]);
    let call = identifier(&event["tool"]["call_id"]);
    let verified = input
        .and_then(|input| crate::ipc::decode::<Value>(input).ok())
        .is_some_and(|native| {
            native == event["native"]
                && session.is_some()
                && call.is_some()
                && matches_native(adapter, event, &native)
        });
    json!({
        "status": if verified { "verified" } else if session.is_some() || call.is_some() { "unverified" } else { "unassociated" },
        "basis": if verified { "native_callback" } else { "none" },
        "session_id": session, "tool_call_id": call,
    })
}

fn matches_native(adapter: &str, event: &Value, native: &Value) -> bool {
    let after = event["name"] == "tool.after";
    let tool = &event["tool"];
    let (session, call, name, input, result, seam) = match adapter {
        "qoder" => (
            native["session_id"].clone(),
            native["tool_use_id"].clone(),
            native["tool_name"].clone(),
            native["tool_input"].clone(),
            native["tool_response"].clone(),
            native["hook_event_name"] == if after { "PostToolUse" } else { "PreToolUse" },
        ),
        "qwenpaw" => (
            native["session_id"].clone(),
            native["tool_call"]["id"].clone(),
            native["tool_call"]["name"].clone(),
            match native["tool_call"]["input"]
                .as_str()
                .and_then(|text| crate::ipc::decode::<Value>(text.as_bytes()).ok())
            {
                Some(value) => value,
                None => return false,
            },
            native["tool_response"].clone(),
            native["event"] == event["name"],
        ),
        "hermes" => {
            let Some(call) = identifier(&native["extra"]["tool_call_id"]) else {
                return false;
            };
            let Some(request) = identifier(&native["extra"]["api_request_id"]) else {
                return false;
            };
            let scoped = canonical::digest(json!([request, call]).to_string().as_bytes());
            (
                native["session_id"].clone(),
                json!(format!("hermes:{scoped}")),
                native["tool_name"].clone(),
                native["tool_input"].clone(),
                native["extra"]["result"].clone(),
                if after {
                    matches!(
                        native["hook_event_name"].as_str(),
                        Some("post_tool_call" | "transform_tool_result")
                    )
                } else {
                    native["hook_event_name"] == "pre_tool_call"
                },
            )
        }
        "openclaw" => {
            let raw = &native["event"];
            let context = &native["context"];
            let Some(call) = raw
                .get("toolCallId")
                .or(context.get("toolCallId"))
                .and_then(identifier)
            else {
                return false;
            };
            let Some(run) = raw
                .get("runId")
                .or(context.get("runId"))
                .and_then(identifier)
            else {
                return false;
            };
            let Some(name) = identifier(&raw["toolName"]) else {
                return false;
            };
            let name = if after {
                match name.trim().to_lowercase().as_str() {
                    "bash" => "exec".into(),
                    "apply-patch" => "apply_patch".into(),
                    "cron" => "automations".into(),
                    other => other.to_owned(),
                }
            } else {
                name.to_owned()
            };
            (
                context["sessionId"].clone(),
                json!(canonical::digest(json!([run, call]).to_string().as_bytes())),
                json!(name),
                raw["params"].clone(),
                raw["result"].clone(),
                native["hook"]
                    == if after {
                        "after_tool_call"
                    } else {
                        "before_tool_call"
                    },
            )
        }
        _ => return false,
    };
    seam && identifier(&session).is_some()
        && identifier(&call).is_some()
        && event["session_id"] == session
        && tool["call_id"] == call
        && tool["native_name"] == name
        && tool["name"] == name
        && tool["input"] == input
        && tool["result"] == result
}
