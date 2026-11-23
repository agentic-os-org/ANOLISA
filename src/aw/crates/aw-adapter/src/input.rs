//! Adopt checked arguments using native envelopes without granting execution permission.

use crate::{HookOutput, NativeExit, Result};
use serde_json::{json, Value};

/// Rebuild callback bytes with revised arguments and unchanged native identity.
///
/// # Errors
/// Rejects an unsupported dialect or non-object arguments.
pub fn native(adapter: &str, original: &Value, input: &Value) -> Result<Vec<u8>> {
    if !input.is_object() {
        return Err("input replacement requires object arguments".into());
    }
    // SDK MCP handlers do not consume native updatedInput replies. Keep them
    // on their existing path rather than checking arguments they will not use.
    if (adapter == "claude-sdk" && original.get("aw_source").is_some())
        || (adapter == "qoder-sdk" && original.get("mcp_context").is_some())
    {
        return Err("SDK MCP handlers do not support pre-tool input replacement".into());
    }
    let mut value = original.clone();
    match adapter {
        "qoder" | "qoder-sdk" | "claude-sdk" | "hermes" => value["tool_input"] = input.clone(),
        // OpenClaw freezes arguments from an earlier approval hook, so a later
        // checked replacement might not be the command it dispatches.
        "openclaw" => return Err("OpenClaw cannot attest final input after approval".into()),
        "pi" => value["tool_call"]["input"] = input.clone(),
        "agentscope" | "qwenpaw" => {
            value["tool_call"]["input"] = json!(serde_json::to_string(input)?);
            value["effective_input"] = input.clone();
        }
        _ => return Err("unsupported input replacement dialect".into()),
    }
    Ok(serde_json::to_vec(&value)?)
}

/// Add checked arguments to a final neutral callback; a veto retains precedence.
///
/// # Errors
/// Rejects a non-before event, unsupported dialect, malformed arguments or native directives.
pub fn reply(
    adapter: &str,
    event: &str,
    mut output: HookOutput,
    input: Value,
) -> Result<HookOutput> {
    if event != "tool.before" || !input.is_object() {
        return Err("invalid input replacement response".into());
    }
    let adopted = crate::reload::adopt_step(adapter, event, "block", output)?;
    if crate::reload::denied(&adopted) {
        return Ok(adopted);
    }
    output = adopted;
    let mut value: Value = if output.stdout.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&output.stdout)?
    };
    if adapter == "qoder" {
        let fields = value
            .as_object_mut()
            .ok_or("invalid native input response")?;
        let specific = fields
            .entry("hookSpecificOutput")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or("invalid native input response")?;
        specific.insert("hookEventName".into(), json!("PreToolUse"));
        specific.insert("updatedInput".into(), input);
        output.stdout = serde_json::to_vec(&value)?;
        return Ok(output);
    }
    if !neutral_final(adapter, &value) {
        return Err("input replacement requires a neutral final native directive".into());
    }
    value = match adapter {
        "qoder" | "qoder-sdk" | "claude-sdk" => {
            json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","updatedInput":input}})
        }
        "openclaw" => json!({"params":input}),
        "hermes" => json!({"action":"modify","args":input,"aw_checked_input":true}),
        "pi" | "agentscope" | "qwenpaw" => json!({"aw_updated_input":input}),
        _ => return Err("unsupported input replacement dialect".into()),
    };
    output.exit = NativeExit::Code(0);
    output.stdout = serde_json::to_vec(&value)?;
    Ok(output)
}

fn neutral_final(adapter: &str, value: &Value) -> bool {
    let Some(fields) = value.as_object() else {
        return false;
    };
    if fields.is_empty() {
        return true;
    }
    match adapter {
        "pi" => {
            fields
                .keys()
                .all(|key| matches!(key.as_str(), "block" | "reason"))
                && fields.get("block").is_none_or(|block| block == false)
                && fields.get("reason").is_none_or(Value::is_string)
        }
        "openclaw" => {
            fields
                .keys()
                .all(|key| matches!(key.as_str(), "block" | "blockReason"))
                && fields.get("block").is_none_or(|block| block == false)
                && fields.get("blockReason").is_none_or(Value::is_string)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sdk_mcp_replacement_is_rejected_before_checks() {
        for (adapter, original) in [
            (
                "claude-sdk",
                json!({"aw_source":"claude-sdk-mcp/v1","tool_input":{"x":1}}),
            ),
            (
                "qoder-sdk",
                json!({"mcp_context":{"server_name":"local"},"tool_input":{"x":1}}),
            ),
        ] {
            assert!(native(adapter, &original, &json!({"x":2})).is_err());
        }
    }

    #[test]
    fn openclaw_replacement_is_rejected_before_checks() {
        let original = json!({"event":{"params":{"command":"original"}}});
        assert!(native("openclaw", &original, &json!({"command":"checked"})).is_err());
    }

    #[test]
    fn neutral_native_allow_does_not_block_checked_input() {
        for (adapter, directive) in [
            ("pi", json!({"block":false})),
            ("pi", json!({"block":false,"reason":"allowed"})),
            ("pi", json!({"reason":"allowed"})),
            ("openclaw", json!({"block":false})),
        ] {
            let output = HookOutput {
                exit: NativeExit::Code(0),
                stdout: serde_json::to_vec(&directive).unwrap(),
                stderr: Vec::new(),
            };
            let revised =
                reply(adapter, "tool.before", output, json!({"command":"checked"})).unwrap();
            let value: Value = serde_json::from_slice(&revised.stdout).unwrap();
            let key = if adapter == "openclaw" {
                "params"
            } else {
                "aw_updated_input"
            };
            assert_eq!(value[key]["command"], "checked");
        }
        for directive in [
            json!({"block":true}),
            json!({"action":"modify","args":{"command":"unchecked"}}),
        ] {
            let output = HookOutput {
                exit: NativeExit::Code(0),
                stdout: serde_json::to_vec(&directive).unwrap(),
                stderr: Vec::new(),
            };
            assert!(reply("pi", "tool.before", output, json!({"command":"checked"})).is_err());
        }
    }

    #[test]
    fn qoder_preserves_native_context_and_denial_without_emitting_approval() {
        let original = json!({"hookSpecificOutput":{"hookEventName":"PreToolUse","additionalContext":"native context"}});
        let output = HookOutput {
            exit: NativeExit::Code(0),
            stdout: serde_json::to_vec(&original).unwrap(),
            stderr: b"diagnostic".to_vec(),
        };
        let revised = reply("qoder", "tool.before", output, json!({"command":"checked"})).unwrap();
        let value: Value = serde_json::from_slice(&revised.stdout).unwrap();
        assert_eq!(
            value["hookSpecificOutput"]["additionalContext"],
            "native context"
        );
        assert_eq!(
            value["hookSpecificOutput"]["updatedInput"]["command"],
            "checked"
        );
        assert!(value["hookSpecificOutput"]
            .get("permissionDecision")
            .is_none());
        assert_eq!(revised.stderr, b"diagnostic");
        let output = HookOutput {
            exit: NativeExit::Code(2),
            stdout: vec![],
            stderr: b"denied".to_vec(),
        };
        let denied = reply("qoder", "tool.before", output, json!({"command":"checked"})).unwrap();
        assert!(matches!(denied.exit, NativeExit::Code(2)));
        assert!(denied.stdout.is_empty());
    }
}
