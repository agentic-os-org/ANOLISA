//! Project one model-visible text slot and preserve its native envelope on replacement.

use super::{adapter::HookOutput, Exit, Result};
use serde_json::{json, Value};

fn result<'a>(adapter: &str, native: &'a Value) -> &'a Value {
    match adapter {
        "deepseek-harness" if native["nested"] != true => &native["tool_response"],
        "qoder" | "qwenpaw" | "pi" | "agentscope" | "codex" => &native["tool_response"],
        "qoder-sdk" if native["hook_event_name"] == "PostToolUse" => &native["tool_response"],
        "claude-sdk"
            if native["hook_event_name"] == "PostToolUse"
                && native["tool_name"] == "Bash"
                && native["tool_response"]["interrupted"] == false
                && native["tool_response"]["isImage"] == false =>
        {
            &native["tool_response"]
        }
        // Native HTTP MCP tools expose the model-visible projection only:
        // the serialized structuredContent string when it is present, or the
        // content blocks when the tool returned plain text.
        "claude-sdk"
            if native["hook_event_name"] == "PostToolUse"
                && native["tool_name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with("mcp__"))
                && matches!(native["tool_response"], Value::String(_) | Value::Array(_)) =>
        {
            &native["tool_response"]
        }
        "openclaw" => &native["event"]["result"],
        "hermes" => &native["extra"]["result"],
        _ => &Value::Null,
    }
}

// Only one text block is replaceable: media and multiple blocks retain their
// native structure and ordering. Qoder/Hermes also expose whole string slots.
fn text_slot<'a>(
    adapter: &str,
    native: &Value,
    result: &'a Value,
) -> Option<(&'static str, &'a str)> {
    if adapter == "claude-sdk" {
        // The bundled CLI adopts a string updatedToolOutput for native MCP
        // tools, replacing the model-visible projection as a whole.
        if native["tool_name"]
            .as_str()
            .is_some_and(|name| name.starts_with("mcp__"))
        {
            if let Some(text) = result.as_str() {
                return Some(("", text));
            }
            return result
                .as_array()
                .filter(|blocks| blocks.len() == 1 && blocks[0]["type"] == "text")
                .and_then(|blocks| blocks[0]["text"].as_str())
                .map(|text| ("", text));
        }
        return result["stdout"].as_str().map(|text| ("/stdout", text));
    }
    if let Some(text) = result.as_str() {
        return Some(("", text));
    }
    if let Some(blocks) = result["content"].as_array() {
        if blocks.len() == 1 && blocks[0]["type"] == "text" {
            return blocks[0]["text"]
                .as_str()
                .map(|text| ("/content/0/text", text));
        }
        return None;
    }
    for field in ["llmContent", "stdout", "output"] {
        if let Some(text) = result[field].as_str() {
            return Some((
                match field {
                    "llmContent" => "/llmContent",
                    "stdout" => "/stdout",
                    _ => "/output",
                },
                text,
            ));
        }
    }
    if adapter == "qoder-sdk" && native["tool_name"] == "Read" && result["type"] == "text" {
        return result["file"]["content"]
            .as_str()
            .map(|text| ("/file/content", text));
    }
    None
}

fn tool<'a>(adapter: &str, native: &'a Value) -> (&'a str, &'a Value) {
    match adapter {
        "qoder" | "qoder-sdk" | "hermes" | "claude-sdk" | "codex" => (
            native["tool_name"].as_str().unwrap_or(""),
            &native["tool_input"],
        ),
        "openclaw" => (
            native["event"]["toolName"].as_str().unwrap_or(""),
            &native["event"]["params"],
        ),
        "pi" | "deepseek-harness" => (
            native["tool_call"]["name"].as_str().unwrap_or(""),
            &native["tool_call"]["input"],
        ),
        "agentscope" => (
            native["tool_call"]["name"].as_str().unwrap_or(""),
            &native["effective_input"],
        ),
        "qwenpaw" => (
            native["tool_call"]["name"].as_str().unwrap_or(""),
            &Value::Null,
        ),
        _ => ("", &Value::Null),
    }
}

fn decoded_result(adapter: &str, native: &Value) -> Value {
    let value = result(adapter, native);
    if adapter == "codex"
        && value["content"].as_array().is_some_and(|blocks| {
            blocks.len() == 1 && blocks[0]["type"] == "text" && blocks[0]["text"].is_string()
        })
        && value["isError"] != true
    {
        return Value::String(value.to_string());
    }
    if adapter == "hermes" {
        if let Some(text) = value.as_str() {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                if parsed["output"].is_string() {
                    return parsed;
                }
            }
        }
    }
    value.clone()
}

/// Recognize failure details before OpenClaw folds them into `isError`.
pub(crate) fn openclaw_result_failed(value: &Value) -> bool {
    let details = &value["details"];
    let status = details["status"]
        .as_str()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let error = match &details["error"] {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        _ => true,
    };
    details["ok"] == false
        || details["success"] == false
        || (matches!(
            status.as_str(),
            "error"
                | "failed"
                | "failure"
                | "timeout"
                | "timed_out"
                | "blocked"
                | "denied"
                | "forbidden"
                | "unavailable"
                | "approval-unavailable"
                | "disabled"
                | "aborted"
                | "cancelled"
                | "canceled"
                | "killed"
                | "invalid"
        ) && details["ok"] != true
            && details["success"] != true)
        || details["timedOut"] == true
        || error
        || details["exitCode"].as_f64().is_some_and(|code| code != 0.0)
}

/// Select the supported native text slot without modifying its envelope.
pub fn project(adapter: &str, native: &Value) -> Option<Value> {
    if adapter == "deepseek-harness" && !crate::adapters::deepseek::replaceable(native) {
        return None;
    }
    if adapter == "claude-sdk" && native.get("aw_source").is_some() {
        return crate::adapters::claude_mcp::project(native);
    }
    let value = decoded_result(adapter, native);
    if adapter == "codex" && !value.is_string() {
        return None;
    }
    let (_, text) = text_slot(adapter, native, &value)?;
    let (name, input) = tool(adapter, native);
    let qwen_input = if adapter == "qwenpaw" {
        serde_json::from_str::<Value>(native["tool_call"]["input"].as_str()?).ok()
    } else {
        None
    };
    let command = qwen_input.as_ref().unwrap_or(input)["command"].as_str();
    // Command evidence takes precedence; names are only a heuristic fallback.
    let origin = if command.is_some() {
        "command_output"
    } else {
        match name.to_ascii_lowercase().as_str() {
            "bash" | "exec" | "terminal" | "execute_shell_command" | "run_shell_command" => {
                "command_output"
            }
            "read" | "read_file" | "read_text_file" | "read_multiple_files" | "file_read" => {
                "file_content"
            }
            _ => "api_response",
        }
    };
    let status = match adapter {
        "qwenpaw" | "agentscope" => value["state"].as_str()?,
        "openclaw" if openclaw_result_failed(&value) => "error",
        "hermes" => match native["extra"]["status"].as_str()? {
            "ok" => "success",
            "error" => "error",
            "blocked" => "denied",
            "interrupted" => "interrupted",
            _ => return None,
        },
        _ => {
            if value["isError"] == true
                || native["event"]["isError"] == true
                || value["exit_code"].as_i64().is_some_and(|code| code != 0)
                || value["error"]
                    .as_str()
                    .is_some_and(|error| !error.is_empty())
                || native["event"]
                    .get("error")
                    .is_some_and(|error| !error.is_null())
            {
                "error"
            } else {
                "success"
            }
        }
    };
    if !matches!(status, "success" | "error" | "denied" | "interrupted") {
        return None;
    }
    let mut projection = json!({"text":text, "status":status, "content_origin":origin});
    if let Some(command) = command {
        projection["command"] = json!(command);
    }
    Some(projection)
}

/// Encode a replacement for a previously authorized native text slot.
pub fn reply(adapter: &str, native: &Value, text: &str) -> Result<HookOutput> {
    if text.is_empty() || text.len() > aw_provider::MAX_MESSAGE_BYTES {
        return Err("replacement text must be nonempty and bounded".into());
    }
    if adapter == "claude-sdk" && native.get("aw_source").is_some() {
        return crate::adapters::claude_mcp::reply(native, text);
    }
    if adapter == "deepseek-harness" && !crate::adapters::deepseek::replaceable(native) {
        return Err("DeepSeek result has no replaceable single-text success slot".into());
    }
    let mut value = decoded_result(adapter, native);
    if adapter == "codex" && !value.is_string() {
        return Err("Codex result has no trusted whole-text projection".into());
    }
    let (pointer, _) =
        text_slot(adapter, native, &value).ok_or("native result has no replaceable text slot")?;
    let pointer = pointer.to_owned();
    *value
        .pointer_mut(&pointer)
        .ok_or("native result slot disappeared")? = json!(text);
    let response = match adapter {
        "codex" => json!({"continue":false,"reason":text}),
        "qoder" | "qoder-sdk" => {
            json!({"hookSpecificOutput":{"hookEventName":"PostToolUse", "updatedToolOutput":
            if let Some(text) = value.as_str() {text.to_owned()} else {serde_json::to_string(&value)?}}})
        }
        "claude-sdk" => {
            json!({"hookSpecificOutput":{"hookEventName":"PostToolUse", "updatedToolOutput":value}})
        }
        "hermes" => {
            json!({"aw_replacement": if let Some(text) = value.as_str() {text.to_owned()} else {serde_json::to_string(&value)?}})
        }
        "openclaw" | "qwenpaw" | "pi" | "agentscope" | "deepseek-harness" => {
            json!({"aw_replacement":value})
        }
        _ => return Err("unsupported result replacement adapter".into()),
    };
    Ok(HookOutput {
        exit: Exit::Code(0),
        stdout: serde_json::to_vec(&response)?,
        stderr: Vec::new(),
    })
}

#[cfg(test)]
mod tests;
