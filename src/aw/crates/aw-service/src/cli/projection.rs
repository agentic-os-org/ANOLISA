//! Project one model-visible text slot and preserve its native envelope on replacement.

use super::{adapter::HookOutput, Exit, Result};
use serde_json::{json, Value};

fn result<'a>(adapter: &str, native: &'a Value) -> &'a Value {
    match adapter {
        "qoder" | "qwenpaw" | "pi" => &native["tool_response"],
        "claude-sdk"
            if native["hook_event_name"] == "PostToolUse"
                && native["tool_name"] == "Bash"
                && native["tool_response"]["interrupted"] == false
                && native["tool_response"]["isImage"] == false =>
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
fn text_slot<'a>(adapter: &str, result: &'a Value) -> Option<(&'static str, &'a str)> {
    if adapter == "claude-sdk" {
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
    None
}

fn tool<'a>(adapter: &str, native: &'a Value) -> (&'a str, &'a Value) {
    match adapter {
        "qoder" | "hermes" | "claude-sdk" => (
            native["tool_name"].as_str().unwrap_or(""),
            &native["tool_input"],
        ),
        "openclaw" => (
            native["event"]["toolName"].as_str().unwrap_or(""),
            &native["event"]["params"],
        ),
        "pi" => (
            native["tool_call"]["name"].as_str().unwrap_or(""),
            &native["tool_call"]["input"],
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

// OpenClaw folds these details into isError only after model-bound middleware.
fn openclaw_result_failed(value: &Value) -> bool {
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

pub(super) fn project(adapter: &str, native: &Value) -> Option<Value> {
    let value = decoded_result(adapter, native);
    let (_, text) = text_slot(adapter, &value)?;
    let (name, input) = tool(adapter, native);
    let origin = match name.to_ascii_lowercase().as_str() {
        "bash" | "exec" | "terminal" | "execute_shell_command" | "run_shell_command" => {
            "command_output"
        }
        "read" | "read_file" | "read_text_file" | "read_multiple_files" | "file_read" => {
            "file_content"
        }
        _ => "api_response",
    };
    let status = match adapter {
        "qwenpaw" => value["state"].as_str()?,
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
    let qwen_input = if adapter == "qwenpaw" {
        serde_json::from_str::<Value>(native["tool_call"]["input"].as_str()?).ok()
    } else {
        None
    };
    if let Some(command) = qwen_input.as_ref().unwrap_or(input)["command"].as_str() {
        projection["command"] = json!(command);
    }
    Some(projection)
}

pub(super) fn reply(adapter: &str, native: &Value, text: &str) -> Result<HookOutput> {
    if text.is_empty() || text.len() > aw_provider::MAX_MESSAGE_BYTES {
        return Err("replacement text must be nonempty and bounded".into());
    }
    let mut value = decoded_result(adapter, native);
    let (pointer, _) =
        text_slot(adapter, &value).ok_or("native result has no replaceable text slot")?;
    let pointer = pointer.to_owned();
    *value
        .pointer_mut(&pointer)
        .ok_or("native result slot disappeared")? = json!(text);
    let response = match adapter {
        "qoder" => json!({"hookSpecificOutput":{"hookEventName":"PostToolUse", "updatedToolOutput":
            if let Some(text) = value.as_str() {text.to_owned()} else {serde_json::to_string(&value)?}}}),
        "claude-sdk" => {
            json!({"hookSpecificOutput":{"hookEventName":"PostToolUse", "updatedToolOutput":value}})
        }
        "hermes" => {
            json!({"aw_replacement": if let Some(text) = value.as_str() {text.to_owned()} else {serde_json::to_string(&value)?}})
        }
        "openclaw" | "qwenpaw" | "pi" => json!({"aw_replacement":value}),
        _ => return Err("unsupported result replacement adapter".into()),
    };
    Ok(HookOutput {
        exit: Exit::Code(0),
        stdout: serde_json::to_vec(&response)?,
        stderr: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_replaces_only_completed_bash_stdout_and_preserves_the_envelope() {
        let native = json!({"hook_event_name":"PostToolUse", "tool_name":"Bash", "tool_input":{"command":"git status"}, "tool_response":{"stdout":"original long output", "stderr":"warning", "interrupted":false, "isImage":false, "content":[{"type":"text","text":"metadata"}], "metadata":{"kept":true}}});
        let projection = project("claude-sdk", &native).unwrap();
        assert_eq!(projection["command"], "git status");
        assert_eq!(projection["status"], "success");
        assert_eq!(projection["text"], "original long output");
        let response: Value =
            serde_json::from_slice(&reply("claude-sdk", &native, "compact").unwrap().stdout)
                .unwrap();
        let mut expected = native["tool_response"].clone();
        expected["stdout"] = json!("compact");
        assert_eq!(
            response,
            json!({"hookSpecificOutput":{"hookEventName":"PostToolUse","updatedToolOutput":expected}})
        );
        for (key, value) in [
            ("hook_event_name", json!("PostToolUseFailure")),
            ("tool_name", json!("mcp__local__Bash")),
            (
                "tool_response",
                json!([{"type":"text","text":"MCP status unknown"}]),
            ),
            (
                "tool_response",
                json!({"stdout":"image", "isImage":true, "interrupted":false}),
            ),
            (
                "tool_response",
                json!({"stdout":"partial", "isImage":false, "interrupted":true}),
            ),
        ] {
            let mut unsupported = native.clone();
            unsupported[key] = value;
            assert!(project("claude-sdk", &unsupported).is_none());
            assert!(reply("claude-sdk", &unsupported, "compact").is_err());
        }
    }

    #[test]
    fn empty_or_unbounded_replacements_are_rejected_for_every_adapter() {
        for adapter in ["qoder", "openclaw", "qwenpaw", "hermes", "pi"] {
            assert!(reply(adapter, &json!({}), "").is_err());
            assert!(reply(
                adapter,
                &json!({}),
                &"x".repeat(aw_provider::MAX_MESSAGE_BYTES + 1)
            )
            .is_err());
        }
    }

    #[test]
    fn pi_projects_object_arguments_and_effective_sdk_error_status() {
        let mut native = json!({"tool_call":{"name":"bash","input":{"command":"git status"}},
            "tool_response":{"content":[{"type":"text","text":"original"}],"isError":false,"details":{"preserved":true}}});
        let projected = project("pi", &native).unwrap();
        assert_eq!(
            projected,
            json!({"text":"original","status":"success","content_origin":"command_output","command":"git status"})
        );
        native["tool_response"]["isError"] = json!(true);
        assert_eq!(project("pi", &native).unwrap()["status"], "error");
        for content in [
            json!([{"type":"image","data":"opaque"}]),
            json!([{"type":"text","text":"a"},{"type":"text","text":"b"}]),
        ] {
            native["tool_response"]["content"] = content;
            assert!(project("pi", &native).is_none());
            assert!(reply("pi", &native, "compact").is_err());
        }
    }

    #[test]
    fn qoder_structured_result_has_an_explicit_string_envelope() {
        let native = json!({"tool_response":{"content":[{"type":"text","text":"original"}],"metadata":{"kept":true}}});
        let encoded: Value =
            serde_json::from_slice(&reply("qoder", &native, "compact").unwrap().stdout).unwrap();
        assert_eq!(
            encoded["hookSpecificOutput"]["updatedToolOutput"],
            r#"{"content":[{"text":"compact","type":"text"}],"metadata":{"kept":true}}"#
        );
    }

    #[test]
    fn openclaw_details_failures_are_not_projected_as_success() {
        for details in [
            json!({"exitCode":127}),
            json!({"status":"completed","exitCode":127}),
            json!({"ok":true,"exitCode":127}),
            json!({"status":"failed"}),
            json!({"timedOut":true}),
            json!({"ok":false}),
            json!({"success":false}),
            json!({"status":" TIMED_OUT "}),
            json!({"status":"denied"}),
            json!({"status":"cancelled"}),
            json!({"status":"completed","error":{"message":"failure"}}),
            json!({"status":"failed","ok":true,"timedOut":true}),
        ] {
            let native = json!({"event":{"result":{"content":[{"type":"text","text":"failure"}],"isError":false,"details":details}}});
            let projection = project("openclaw", &native).unwrap();
            assert_eq!(projection["status"], "error", "{details}");
            assert_eq!(projection["text"], "failure");
        }
        for details in [
            json!(null),
            json!({}),
            json!({"exitCode":0,"timedOut":false,"ok":true}),
            json!({"status":"failed","ok":true}),
            json!({"status":"error","success":true}),
            json!({"error":false}),
            json!({"error":0}),
            json!({"error":""}),
        ] {
            let native = json!({"event":{"result":{"content":[{"type":"text","text":"original"}],"details":details}}});
            assert_eq!(
                project("openclaw", &native).unwrap()["status"],
                "success",
                "{details}"
            );
        }
    }

    #[test]
    fn text_replacement_preserves_native_metadata_and_does_not_add_a_copy() {
        for adapter in ["openclaw", "qwenpaw", "pi"] {
            let original = json!({"content":[{"type":"text","text":"original","annotations":{"kept":true}}],"metadata":{"kept":42},"state":"success"});
            let native = if adapter == "openclaw" {
                json!({"event":{"result":original}})
            } else {
                json!({"tool_response":original})
            };
            let reply: Value =
                serde_json::from_slice(&reply(adapter, &native, "compressed").unwrap().stdout)
                    .unwrap();
            let mut expected = original;
            expected["content"][0]["text"] = json!("compressed");
            assert_eq!(reply["aw_replacement"], expected);
        }
    }

    #[test]
    fn media_and_multiple_blocks_have_no_projection() {
        for content in [
            json!([{"type":"image","data":"opaque"}]),
            json!([{"type":"text","text":"a"},{"type":"text","text":"b"}]),
        ] {
            assert!(
                project("openclaw", &json!({"event":{"result":{"content":content}}})).is_none()
            );
        }
    }

    #[test]
    fn hermes_preserves_terminal_envelope_and_file_origin() {
        let native = json!({"tool_name":"terminal", "tool_input":{"command":"cat file.json"}, "extra":{"status":"ok", "result":"{\"output\":\"original\",\"exit_code\":0}"}});
        assert_eq!(
            project("hermes", &native).unwrap()["command"],
            "cat file.json"
        );
        let response: Value =
            serde_json::from_slice(&reply("hermes", &native, "compact").unwrap().stdout).unwrap();
        assert_eq!(
            response["aw_replacement"],
            r#"{"exit_code":0,"output":"compact"}"#
        );
        let result: Value =
            serde_json::from_str(response["aw_replacement"].as_str().unwrap()).unwrap();
        assert_eq!(result, json!({"output":"compact","exit_code":0}));
    }
}
