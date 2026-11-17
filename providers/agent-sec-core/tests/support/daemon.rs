#![allow(dead_code)]

use aw_provider::{Protocol, VERSION};
use serde_json::{json, Value};
use std::path::PathBuf;

pub fn bind_req(val: Value) -> Value {
    Protocol::new()
        .unwrap()
        .bind_invocation(val)
        .unwrap()
        .as_value()
        .clone()
}

pub fn scan_code_request(code: &str, budget_ms: u64) -> Value {
    bind_req(json!({
        "api_version": VERSION,
        "request_id": "req-code-1",
        "method": "invoke",
        "operation": "scan_code",
        "config_revision": "b".repeat(64),
        "budget_ms": budget_ms,
        "allowed_effects": ["observe", "block"],
        "config": {
            "version": 1,
            "mode": "block",
            "tools": {"Bash": {"language": "bash", "input_pointer": "/command"}}
        },
        "event": {
            "name": "tool.before",
            "agent": {"adapter": "qoder", "binding_id": "qoder", "instance_id": "inst-1"},
            "session_id": "aw-sess",
            "tool": {"name": "Bash", "native_name": "Bash", "call_id": "aw-call", "input": {"command": code}, "result": null},
            "native": {
                "trace_id": "trace-abc-123",
                "session_id": "sess-1",
                "run_id": "run-1",
                "tool_use_id": "native-tool-9"
            }
        }
    }))
}

pub fn scan_prompt_request(text: &str, budget_ms: u64) -> Value {
    bind_req(json!({
        "api_version": VERSION,
        "request_id": "req-prompt-1",
        "method": "invoke",
        "operation": "scan_prompt",
        "config_revision": "b".repeat(64),
        "budget_ms": budget_ms,
        "allowed_effects": ["observe", "block"],
        "config": {"version": 1, "mode": "block", "prompt": {"mode": "block", "scan_mode": "fast"}},
        "event": {
            "name": "input.submit",
            "agent": {"adapter": "qoder-sdk", "binding_id": "sdk", "instance_id": "inst-1"},
            "session_id": "sess-p1",
            "turn_id": "turn-p1",
            "input": {"text": text},
            "native": {}
        }
    }))
}

pub fn scan_user_pii_request(text: &str, budget_ms: u64) -> Value {
    bind_req(json!({
        "api_version": VERSION,
        "request_id": "req-pii-1",
        "method": "invoke",
        "operation": "scan_user_input",
        "config_revision": "b".repeat(64),
        "budget_ms": budget_ms,
        "allowed_effects": ["observe", "block"],
        "config": {"version": 1, "mode": "block", "pii": {"input_mode": "block", "output_mode": "observe"}},
        "event": {
            "name": "input.submit",
            "agent": {"adapter": "qoder-sdk", "binding_id": "sdk", "instance_id": "inst-1"},
            "session_id": "sess-u1",
            "turn_id": "turn-u1",
            "input": {"text": text},
            "native": {}
        }
    }))
}

pub fn scan_tool_input_pii_request(text: &str, budget_ms: u64) -> Value {
    bind_req(json!({
        "api_version": VERSION,
        "request_id": "req-tool-pii-in",
        "method": "invoke",
        "operation": "scan_tool_input",
        "config_revision": "b".repeat(64),
        "budget_ms": budget_ms,
        "allowed_effects": ["observe", "block"],
        "config": {"version": 1, "mode": "block", "pii": {"input_mode": "block", "output_mode": "replace_result"}},
        "event": {
            "name": "tool.before",
            "agent": {"adapter": "qoder", "binding_id": "qoder", "instance_id": "inst-1"},
            "session_id": "sess-ti1",
            "tool": {
                "name": "Bash",
                "native_name": "Bash",
                "call_id": "call-ti1",
                "input": {"command": text},
                "result": null
            },
            "native": {}
        }
    }))
}

pub fn scan_tool_output_pii_request(text: &str, budget_ms: u64) -> Value {
    bind_req(json!({
        "api_version": VERSION,
        "request_id": "req-tool-pii-out",
        "method": "invoke",
        "operation": "scan_tool_output",
        "config_revision": "b".repeat(64),
        "budget_ms": budget_ms,
        "allowed_effects": ["observe", "replace_result"],
        "config": {"version": 1, "mode": "block", "pii": {"input_mode": "block", "output_mode": "replace_result"}},
        "event": {
            "name": "tool.after",
            "agent": {"adapter": "qoder", "binding_id": "qoder", "instance_id": "inst-1"},
            "session_id": "sess-to1",
            "tool": {
                "name": "Bash",
                "native_name": "Bash",
                "call_id": "call-to1",
                "input": {"command": "echo out"},
                "result": {"content": [{"type": "text", "text": text}], "state": "error"},
                "result_projection": {"text": text, "status": "error", "content_origin": "file_content"}
            },
            "native": {}
        }
    }))
}

pub fn check_final_tool_output_pii_request(text: &str, budget_ms: u64) -> Value {
    bind_req(json!({
        "api_version": VERSION,
        "request_id": "req-final-pii-out",
        "method": "invoke",
        "operation": "check_final_tool_output",
        "config_revision": "b".repeat(64),
        "budget_ms": budget_ms,
        "allowed_effects": ["observe", "block"],
        "config": {"version": 1, "mode": "block", "pii": {"input_mode": "block", "output_mode": "replace_result"}},
        "event": {
            "name": "security.violation",
            "agent": {"adapter": "deepseek-harness", "binding_id": "dsh", "instance_id": "inst-1"},
            "session_id": "sess-final1",
            "tool": {
                "name": "Bash",
                "native_name": "Bash",
                "call_id": "call-final1",
                "input": {"command": "echo out"},
                "result": {"content": [{"type": "text", "text": text}], "state": "error"},
                "result_projection": {"text": text, "status": "error", "content_origin": "file_content"}
            },
            "native": {}
        }
    }))
}

pub fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("aw-dc-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}
