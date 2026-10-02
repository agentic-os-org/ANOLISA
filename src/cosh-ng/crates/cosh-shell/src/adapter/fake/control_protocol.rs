use std::thread;
use std::time::Duration;

use crate::types::{AgentEvent, AgentRequest};

use super::AdapterError;

pub(super) fn emit_fake_control_protocol_stream(
    input: &str,
    request: &AgentRequest,
    sink: &mut dyn FnMut(AgentEvent) -> Result<(), AdapterError>,
) -> Result<bool, AdapterError> {
    if input.contains("ShellCommandCompleted evidence") {
        return Ok(false);
    }

    let run_id = request.id.clone();
    if input.contains("provider native tool") {
        sink(AgentEvent::StatusChanged {
            run_id: run_id.clone(),
            phase: "routing".to_string(),
            message: "matching provider-native fake tool workflow".to_string(),
        })?;
        sink(AgentEvent::ToolPermissionRequest {
            run_id: run_id.clone(),
            request_id: "ctrl-1".to_string(),
            tool_name: "run_shell_command".to_string(),
            tool_input: serde_json::json!({ "command": "printf 'provider-shell-handoff\\n'" }),
            tool_use_id: "toolu-1".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        })?;
        thread::sleep(Duration::from_millis(800));
        sink(AgentEvent::ToolOutputDelta {
            run_id: run_id.clone(),
            tool_id: "toolu-1".to_string(),
            stream: "stdout".to_string(),
            text: "PROVIDER NATIVE OUTPUT RENDERED AFTER ALLOW\n".to_string(),
        })?;
        sink(AgentEvent::ToolCompleted {
            run_id: run_id.clone(),
            tool_id: "toolu-1".to_string(),
            status: "completed".to_string(),
        })?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "provider-native fake tool completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("provider memory hook shell") {
        sink(AgentEvent::ToolPermissionRequest {
            run_id: run_id.clone(),
            request_id: "ctrl-memory-hook-1".to_string(),
            tool_name: "run_shell_command".to_string(),
            tool_input: serde_json::json!({ "command": "free -m" }),
            tool_use_id: "toolu-memory-hook-1".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        })?;
        thread::sleep(Duration::from_millis(800));
        sink(AgentEvent::ToolOutputDelta {
            run_id: run_id.clone(),
            tool_id: "toolu-memory-hook-1".to_string(),
            stream: "stdout".to_string(),
            text: "PROVIDER MEMORY NATIVE OUTPUT SHOULD NOT RENDER AFTER ALLOW\n".to_string(),
        })?;
        sink(AgentEvent::ToolCompleted {
            run_id: run_id.clone(),
            tool_id: "toolu-memory-hook-1".to_string(),
            status: "completed".to_string(),
        })?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "provider memory hook fake tool completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("provider resume timeout shell") {
        let command = if input.contains("structured before recovery") {
            "printf structured-before-recovery"
        } else {
            "ssh -V"
        };
        sink(AgentEvent::ToolPermissionRequest {
            run_id,
            request_id: "ctrl-timeout-1".to_string(),
            tool_name: "run_shell_command".to_string(),
            tool_input: serde_json::json!({ "command": command }),
            tool_use_id: "toolu-timeout-1".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        })?;
        return Ok(true);
    }

    if input.contains("provider auto safe shell") {
        sink(AgentEvent::ToolPermissionRequest {
            run_id: run_id.clone(),
            request_id: "ctrl-auto-1".to_string(),
            tool_name: "run_shell_command".to_string(),
            tool_input: serde_json::json!({ "command": "df -h" }),
            tool_use_id: "toolu-auto-1".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        })?;
        thread::sleep(Duration::from_millis(800));
        sink(AgentEvent::ToolOutputDelta {
            run_id: run_id.clone(),
            tool_id: "toolu-auto-1".to_string(),
            stream: "stdout".to_string(),
            text: "PROVIDER AUTO NATIVE OUTPUT RENDERED AFTER ALLOW\n".to_string(),
        })?;
        sink(AgentEvent::ToolCompleted {
            run_id: run_id.clone(),
            tool_id: "toolu-auto-1".to_string(),
            status: "completed".to_string(),
        })?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "provider auto fake tool completed".to_string(),
        })?;
        return Ok(true);
    }

    if input.contains("provider tty shell") {
        sink(AgentEvent::ToolPermissionRequest {
            run_id: run_id.clone(),
            request_id: "ctrl-tty-risk-1".to_string(),
            tool_name: "run_shell_command".to_string(),
            tool_input: serde_json::json!({ "command": "ssh -V" }),
            tool_use_id: "toolu-tty-risk-1".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        })?;
        thread::sleep(Duration::from_millis(800));
        sink(AgentEvent::ToolOutputDelta {
            run_id: run_id.clone(),
            tool_id: "toolu-tty-risk-1".to_string(),
            stream: "stdout".to_string(),
            text: "PROVIDER TTY OUTPUT SHOULD NOT RENDER AFTER RECOVERY\n".to_string(),
        })?;
        sink(AgentEvent::ToolCompleted {
            run_id: run_id.clone(),
            tool_id: "toolu-tty-risk-1".to_string(),
            status: "completed".to_string(),
        })?;
        sink(AgentEvent::AgentCompleted {
            run_id,
            summary: "provider tty fake tool completed".to_string(),
        })?;
        return Ok(true);
    }

    Ok(false)
}
