//! Shared validation for lifecycle reasons supplied by an admitted producer.

use crate::{NativeContext, Result};
use serde_json::{json, Value};

pub(super) fn reason(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err("lifecycle reason must be nonempty and at most 512 bytes".into());
    }
    Ok(())
}

fn identity(value: &Value) -> Result<&str> {
    let value = value.as_str().ok_or("missing lifecycle identity")?;
    reason(value)?;
    Ok(value)
}

pub(crate) fn normalize(
    context: &NativeContext,
    adapter: &str,
    event: &str,
    native: &Value,
) -> Result<Value> {
    let session = identity(&native["session_id"])?;
    let mut output = json!({"name":event, "session_id":session,
        "agent":{"adapter":adapter,"binding_id":context.target,"instance_id":context.instance_id},
        "native":native});
    match event {
        "session.start" | "session.end" => {
            if let Some(epoch) = native.get("session_epoch") {
                identity(epoch)?;
            }
            if event == "session.end" {
                reason(
                    native["reason"]
                        .as_str()
                        .ok_or("missing session end reason")?,
                )?;
            }
        }
        "turn.stop" => {
            output["turn_id"] = json!(identity(&native["turn_id"])?);
            for field in ["attempt_id", "delivery_id"] {
                if let Some(value) = native.get(field) {
                    identity(value)?;
                }
            }
            reason(
                native["reason"]
                    .as_str()
                    .ok_or("missing turn stop reason")?,
            )?;
        }
        _ => return Err("unsupported lifecycle event".into()),
    }
    Ok(output)
}

pub(super) const SEMANTIC_SDK: &[&str] = &[
    "session.start",
    "input.submit",
    "tool.before",
    "tool.after",
    "turn.stop",
    "session.end",
];

pub(super) const QODER_SDK: &[&str] = &[
    "session.start",
    "input.submit",
    "tool.before",
    "tool.after",
    "security.violation",
    "session.end",
];

pub(super) const CLAUDE_SDK: &[&str] = &[
    "session.start",
    "session.end",
    "input.submit",
    "tool.before",
    "tool.after",
    "security.violation",
    "turn.stop",
];
