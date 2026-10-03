//! JSON encoding for requests and responses on the provider control channel.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::{ShellEvidenceResult, CONTROL_PROTOCOL_VERSION};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExecutedShellResult {
    pub llm_content: String,
    pub return_display: Option<String>,
    pub metadata: HostExecutedShellMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExecutedShellMetadata {
    pub command: String,
    pub status: String,
    pub exit_code: i32,
    pub signal: Option<String>,
    pub cwd: String,
    pub end_cwd: String,
    pub duration_ms: u64,
    pub output_ref: Option<String>,
    pub redaction_status: String,
    pub approval_id: Option<String>,
    pub tool_use_id: Option<String>,
    /// #2161: kernel-evidenced input-wait facts observed while the command
    /// ran; omitted entirely when no wait was detected (back compat).
    pub input_wait: Option<HostExecutedInputWait>,
}

/// #2161 contract annotation: lets the provider know the command sat in an
/// interactive input-wait, and whether the input-wait timeout interrupted
/// it — actionable signal to retry non-interactively (`</dev/null`,
/// prompt-skipping flags, or piping an answer).
///
/// Granularity contract (#2168 review): facts aggregate over the whole
/// approval/handoff — the provider's retry unit. `waited_secs` is the
/// longest single uninterrupted wait episode observed while the handoff
/// ran; `interrupted=true` means the foreground group was interrupted at
/// that point, so nothing after the interrupt executed. Steps that
/// completed before the interrupt are reflected in the command's captured
/// output/exit metadata, not re-attributed per sub-command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostExecutedInputWait {
    pub waited_secs: u64,
    pub interrupted: bool,
}

pub fn serialize_initialize(request_id: &str) -> String {
    json!({
        "request_id": request_id,
        "type": "control_request",
        "request": { "subtype": "initialize" }
    })
    .to_string()
}

/// Serializes one-shot shell initialization without SessionStart hooks.
pub(crate) fn serialize_initialize_without_session_start(request_id: &str) -> String {
    json!({
        "request_id": request_id,
        "type": "control_request",
        "request": {
            "subtype": "initialize",
            "fire_session_start": false,
            "protocol_version": CONTROL_PROTOCOL_VERSION,
        }
    })
    .to_string()
}

/// Encodes the versioned initialize handshake used by persistent cosh-core,
/// declaring the control capabilities this adapter implements. The core only
/// moves trust-mode shell execution onto the approval channel when both are
/// declared (#2067); foreign drivers (claude/qwen) keep the plain payload.
pub fn serialize_cosh_core_initialize(request_id: &str) -> String {
    json!({
        "request_id": request_id,
        "type": "control_request",
        "request": {
            "subtype": "initialize",
            "protocol_version": CONTROL_PROTOCOL_VERSION,
            "capabilities": {
                "can_handle_can_use_tool": true,
                "can_handle_host_executed_shell": true
            }
        }
    })
    .to_string()
}

pub fn serialize_user_message(content: &str, session_id: Option<&str>) -> String {
    let mut message = json!({
        "type": "user",
        "message": { "role": "user", "content": content },
        "parent_tool_use_id": null
    });
    if let Some(session_id) = session_id {
        message["session_id"] = Value::String(session_id.to_string());
    }
    message.to_string()
}

/// Serializes a cosh-core user message with the raw input used by hooks.
///
/// The field is omitted when unavailable so older cores and direct callers
/// retain the original payload shape and continue to fall back to `content`.
pub(crate) fn serialize_cosh_core_user_message(
    content: &str,
    raw_user_input: Option<&str>,
    session_id: Option<&str>,
) -> String {
    let mut message = json!({
        "type": "user",
        "message": { "role": "user", "content": content },
        "parent_tool_use_id": null
    });
    if let Some(raw_user_input) = raw_user_input {
        message["message"]["raw_user_input"] = Value::String(raw_user_input.to_string());
    }
    if let Some(session_id) = session_id {
        message["session_id"] = Value::String(session_id.to_string());
    }
    message.to_string()
}

pub fn serialize_co_allow(request_id: &str) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "allow"
            }
        }
    })
    .to_string()
}

/// #1940 receipt protocol: emitted as soon as a control approval request
/// reaches the shell main thread. A core that announced
/// `can_handle_approval_receipt` disarms its residual timeout for this
/// request; writers skip the line entirely for providers without the
/// capability, whose guard then stays armed.
pub(crate) fn serialize_approval_receipt(request_id: &str) -> String {
    json!({
        "type": "approval_receipt",
        "request_id": request_id
    })
    .to_string()
}

pub fn serialize_claude_allow(request_id: &str, updated_input: &Value) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "allow",
                "updatedInput": updated_input
            }
        }
    })
    .to_string()
}

pub fn serialize_deny(request_id: &str, message: &str) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "deny",
                "message": message
            }
        }
    })
    .to_string()
}

pub fn serialize_host_executed_shell_result(
    request_id: &str,
    result: &HostExecutedShellResult,
) -> String {
    let mut metadata = json!({
        "command": result.metadata.command,
        "status": result.metadata.status,
        "exit_code": result.metadata.exit_code,
        "signal": result.metadata.signal,
        "cwd": result.metadata.cwd,
        "end_cwd": result.metadata.end_cwd,
        "duration_ms": result.metadata.duration_ms,
        "output_ref": result.metadata.output_ref,
        "redaction_status": result.metadata.redaction_status,
        "approval_id": result.metadata.approval_id,
        "tool_use_id": result.metadata.tool_use_id,
    });
    if let Some(input_wait) = &result.metadata.input_wait {
        let mut facts = json!({
            "detected": true,
            "waited_secs": input_wait.waited_secs,
            "interrupted": input_wait.interrupted,
        });
        if input_wait.interrupted {
            facts["reason"] = Value::String("input-wait-timeout".to_string());
        }
        metadata["input_wait"] = facts;
    }
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "host_executed_shell",
                "result": {
                    "llmContent": result.llm_content,
                    "returnDisplay": result.return_display,
                    "metadata": metadata
                }
            }
        }
    })
    .to_string()
}

pub fn serialize_shell_evidence_result(request_id: &str, result: &ShellEvidenceResult) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "behavior": "shell_evidence",
                "result": {
                    "llmContent": result.llm_content,
                    "returnDisplay": result.return_display,
                    "metadata": {
                        "action": result.metadata.action,
                        "scope": result.metadata.scope,
                        "limit": result.metadata.limit,
                        "next_cursor": result.metadata.next_cursor,
                        "output_id": result.metadata.output_id,
                        "status": result.metadata.status,
                        "excerpt_status": result.metadata.excerpt_status,
                        "reason": result.metadata.reason,
                        "direction": result.metadata.direction,
                        "lines": result.metadata.lines,
                        "command_count": result.metadata.command_count,
                        "provider_visible_byte_cap": result.metadata.provider_visible_byte_cap,
                        "truncated": result.metadata.truncated,
                        "truncated_by_lines": result.metadata.truncated_by_lines,
                        "truncated_by_bytes": result.metadata.truncated_by_bytes,
                        "truncation_reason": result.metadata.truncation_reason,
                        "is_error": result.metadata.is_error,
                    }
                }
            }
        }
    })
    .to_string()
}

pub fn serialize_answer(request_id: &str, answer: &str) -> String {
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "answer": answer
            }
        }
    })
    .to_string()
}

pub fn serialize_auth_response(
    request_id: &str,
    provider_id: &str,
    provider_type: Option<&str>,
    values: &HashMap<String, String>,
    persist: bool,
) -> String {
    let values_json: Value = values
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect::<serde_json::Map<String, Value>>()
        .into();
    json!({
        "type": "control_response",
        "response": {
            "subtype": "success",
            "request_id": request_id,
            "response": {
                "provider_id": provider_id,
                "provider_type": provider_type,
                "values": values_json,
                "persist": persist
            }
        }
    })
    .to_string()
}
