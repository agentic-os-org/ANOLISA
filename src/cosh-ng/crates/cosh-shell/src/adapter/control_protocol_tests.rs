use std::collections::HashMap;
use std::time::Duration;

use serde_json::{json, Value};

use super::control_protocol::*;
use crate::types::{AgentEvent, QuestionSelectionMode};

#[test]
fn parse_can_use_tool() {
    let line = r#"{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"echo hello"},"tool_use_id":"toolu_xxx"}}"#;
    let req = parse_control_request(line).expect("should parse");
    match req {
        ControlRequest::CanUseTool {
            request_id,
            tool_name,
            tool_input,
            tool_use_id,
            hook_requires_approval,
            ..
        } => {
            assert_eq!(request_id, "req-1");
            assert_eq!(tool_name, "Bash");
            assert_eq!(tool_input["command"], "echo hello");
            assert_eq!(tool_use_id, "toolu_xxx");
            assert!(!hook_requires_approval);
        }
        _ => panic!("expected CanUseTool"),
    }
}

#[test]
fn parse_can_use_tool_preserves_audit_reference() {
    let line = r#"{"type":"control_request","request_id":"req-1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"echo hello"},"tool_use_id":"toolu_xxx","audit_ref":"audit-event-1"}}"#;
    match parse_control_request(line).expect("parse audit-linked approval") {
        ControlRequest::CanUseTool { audit_ref, .. } => {
            assert_eq!(audit_ref.as_deref(), Some("audit-event-1"));
        }
        _ => panic!("expected CanUseTool"),
    }
}

#[test]
fn parse_initialize() {
    let line =
        r#"{"request_id":"init-1","type":"control_request","request":{"subtype":"initialize"}}"#;
    let req = parse_control_request(line).expect("should parse");
    match req {
        ControlRequest::Initialize { request_id } => {
            assert_eq!(request_id, "init-1");
        }
        _ => panic!("expected Initialize"),
    }
}

#[test]
fn parse_ask_user() {
    let line = r#"{"type":"control_request","request_id":"ask-1","request":{"subtype":"ask_user","question":"Pick one","options":[{"label":"Blue"},{"label":"Green"}],"allow_free_text":false,"multi_select":true}}"#;
    let req = parse_control_request(line).expect("should parse");
    match req {
        ControlRequest::AskUser {
            request_id,
            question,
            options,
            allow_free_text,
            selection_mode,
        } => {
            assert_eq!(request_id, "ask-1");
            assert_eq!(question, "Pick one");
            assert_eq!(options, ["Blue", "Green"]);
            assert!(!allow_free_text);
            assert_eq!(selection_mode, QuestionSelectionMode::Multiple);
        }
        _ => panic!("expected AskUser"),
    }
}

#[test]
fn parse_read_shell_output_subtype_is_not_final_protocol() {
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"read_shell_output","tool_use_id":"toolu_abc","output_id":"terminal-output://raw-session-a1b2/cmd-1","direction":"tail","lines":120}}"#
    )
    .is_none());
}

#[test]
fn parse_shell_evidence_list_commands() {
    let line = r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands"}}"#;
    let req = parse_control_request(line).expect("should parse");
    match req {
        ControlRequest::ShellEvidence {
            request_id,
            tool_use_id,
            action,
        } => {
            assert_eq!(request_id, "evidence-1");
            assert_eq!(tool_use_id, "toolu_abc");
            assert_eq!(
                action,
                ShellEvidenceAction::ListCommands {
                    limit: 20,
                    cursor: None
                }
            );
            assert_eq!(action.as_str(), "list_commands");
        }
        _ => panic!("expected ShellEvidence"),
    }
}

#[test]
fn parse_shell_evidence_list_commands_pagination() {
    let line = r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands","limit":2,"cursor":"offset:2"}}"#;
    let req = parse_control_request(line).expect("should parse");
    match req {
        ControlRequest::ShellEvidence {
            action: ShellEvidenceAction::ListCommands { limit, cursor },
            ..
        } => {
            assert_eq!(limit, 2);
            assert_eq!(cursor.as_deref(), Some("offset:2"));
        }
        _ => panic!("expected ShellEvidence list_commands"),
    }
}

#[test]
fn parse_shell_evidence_read_output() {
    let line = r#"{"type":"control_request","request_id":"evidence-2","request":{"subtype":"shell_evidence","tool_use_id":"toolu_def","action":"read_output","output_id":"terminal-output://raw-session-a1b2/cmd-1","direction":"head","lines":12}}"#;
    let req = parse_control_request(line).expect("should parse");
    match req {
        ControlRequest::ShellEvidence {
            request_id,
            tool_use_id,
            action:
                ShellEvidenceAction::ReadOutput {
                    output_id,
                    direction,
                    lines,
                    bypass_recent_filter,
                },
        } => {
            assert_eq!(request_id, "evidence-2");
            assert_eq!(tool_use_id, "toolu_def");
            assert_eq!(output_id, "terminal-output://raw-session-a1b2/cmd-1");
            assert_eq!(direction, ShellOutputDirection::Head);
            assert_eq!(lines, 12);
            assert!(!bypass_recent_filter);
        }
        _ => panic!("expected ShellEvidence read_output"),
    }
}

#[test]
fn parse_shell_evidence_read_output_defaults_optional_fields() {
    let line = r#"{"type":"control_request","request_id":"evidence-2","request":{"subtype":"shell_evidence","tool_use_id":"toolu_def","action":"read_output","output_id":"terminal-output://raw-session-a1b2/cmd-1"}}"#;
    let req = parse_control_request(line).expect("should parse");
    match req {
        ControlRequest::ShellEvidence {
            action:
                ShellEvidenceAction::ReadOutput {
                    direction,
                    lines,
                    bypass_recent_filter,
                    ..
                },
            ..
        } => {
            assert_eq!(direction, ShellOutputDirection::Tail);
            assert_eq!(lines, 120);
            assert!(!bypass_recent_filter);
        }
        _ => panic!("expected ShellEvidence read_output"),
    }
}

#[test]
fn parse_shell_evidence_read_output_bypass_recent_filter() {
    let line = r#"{"type":"control_request","request_id":"evidence-2","request":{"subtype":"shell_evidence","tool_use_id":"toolu_def","action":"read_output","output_id":"terminal-output://raw-session-a1b2/cmd-1","bypass_recent_filter":true}}"#;
    let req = parse_control_request(line).expect("should parse");
    match req {
        ControlRequest::ShellEvidence {
            action:
                ShellEvidenceAction::ReadOutput {
                    bypass_recent_filter,
                    ..
                },
            ..
        } => assert!(bypass_recent_filter),
        _ => panic!("expected ShellEvidence read_output"),
    }
}

#[test]
fn parse_shell_evidence_list_commands_ignores_direction_hint() {
    let line = r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands","direction":"tail","limit":10}}"#;
    let req = parse_control_request(line).expect("should parse");
    match req {
        ControlRequest::ShellEvidence {
            action: ShellEvidenceAction::ListCommands { limit, cursor },
            ..
        } => {
            assert_eq!(limit, 10);
            assert_eq!(cursor, None);
        }
        _ => panic!("expected ShellEvidence list_commands"),
    }
}

#[test]
fn parse_shell_evidence_rejects_invalid_action_family() {
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"read_shell_output","output_id":"terminal-output://raw-session/cmd-1"}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"read_output","output_id":"/tmp/file"}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","action":"list_commands"}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"read_output","output_id":"terminal-output://raw-session/cmd-1","direction":"middle","lines":120}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"read_output","output_id":"terminal-output://raw-session/cmd-1","direction":"tail","lines":0}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands","output_id":"terminal-output://raw-session/cmd-1"}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands","lines":120}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands","bypass_recent_filter":true}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands","limit":0}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands","limit":"many"}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands","cursor":3}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"read_output","output_id":"terminal-output://raw-session/cmd-1","direction":7}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"read_output","output_id":"terminal-output://raw-session/cmd-1","lines":"many"}}"#
    )
    .is_none());
    assert!(parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"read_output","output_id":"terminal-output://raw-session/cmd-1","bypass_recent_filter":"yes"}}"#
    )
    .is_none());
}

#[test]
fn parse_shell_evidence_caps_limit_and_lines() {
    let req = parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-1","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"list_commands","limit":101}}"#
    )
    .expect("should parse");
    match req {
        ControlRequest::ShellEvidence {
            action: ShellEvidenceAction::ListCommands { limit, .. },
            ..
        } => assert_eq!(limit, 100),
        _ => panic!("expected list_commands"),
    }

    let req = parse_control_request(
        r#"{"type":"control_request","request_id":"evidence-2","request":{"subtype":"shell_evidence","tool_use_id":"toolu_abc","action":"read_output","output_id":"terminal-output://raw-session/cmd-1","lines":301}}"#
    )
    .expect("should parse");
    match req {
        ControlRequest::ShellEvidence {
            action: ShellEvidenceAction::ReadOutput { lines, .. },
            ..
        } => assert_eq!(lines, 300),
        _ => panic!("expected read_output"),
    }
}

#[test]
fn parse_non_control_request_returns_none() {
    assert!(parse_control_request(r#"{"type":"assistant","message":"hi"}"#).is_none());
    assert!(parse_control_request(r#"{"type":"result","result":"done"}"#).is_none());
    assert!(parse_control_request("not json at all").is_none());
    assert!(parse_control_request("").is_none());
}

#[test]
fn parse_versioned_initialize_response() {
    let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","protocol_version":1,"capabilities":{"can_handle_can_use_tool":true,"can_handle_host_executed_shell_tool_result":true,"can_handle_shell_evidence_tool":true,"can_handle_approval_receipt":true}}}}"#;
    let capabilities = parse_initialize_response(line, "init-1")
        .expect("initialize response")
        .expect("compatible version");
    assert!(capabilities.provider_initialize_seen);
    assert_eq!(
        capabilities.protocol_version,
        Some(CONTROL_PROTOCOL_VERSION)
    );
    assert!(capabilities.can_handle_can_use_tool);
    assert!(capabilities.can_handle_host_executed_shell_tool_result);
    assert!(capabilities.can_handle_shell_evidence_tool);
    assert!(capabilities.can_handle_approval_receipt);
}

#[test]
fn parse_legacy_initialize_response_defaults_capabilities() {
    let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","capabilities":{}}}}"#;
    let capabilities = parse_initialize_response(line, "init-1")
        .expect("initialize response")
        .expect("legacy compatibility");
    assert!(capabilities.provider_initialize_seen);
    assert_eq!(capabilities.protocol_version, None);
    assert!(!capabilities.can_handle_can_use_tool);
    assert!(!capabilities.can_handle_host_executed_shell_tool_result);
    assert!(!capabilities.can_handle_shell_evidence_tool);
    assert!(!capabilities.can_handle_approval_receipt);
}

#[test]
fn receipt_capable_requires_the_announced_capability() {
    use std::sync::{Arc, Mutex};
    let capabilities = Arc::new(Mutex::new(ControlProtocolCapabilities::default()));
    assert!(!receipt_capable(&capabilities));
    capabilities.lock().unwrap().can_handle_approval_receipt = true;
    assert!(receipt_capable(&capabilities));
}

#[test]
fn parse_initialize_response_rejects_incompatible_version() {
    let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"init-1","response":{"subtype":"initialize","protocol_version":9,"capabilities":{}}}}"#;
    let error = parse_initialize_response(line, "init-1")
        .expect("initialize response")
        .expect_err("unsupported version");
    assert!(error.contains("unsupported control protocol version 9"));
}

#[test]
fn parse_initialize_response_rejects_mismatched_request_id() {
    let line = r#"{"type":"control_response","response":{"subtype":"success","request_id":"wrong","response":{"subtype":"initialize","protocol_version":1,"capabilities":{}}}}"#;
    let error = parse_initialize_response(line, "init-1")
        .expect("initialize response")
        .expect_err("mismatched request id");
    assert!(error.contains("does not match"));
}

#[test]
fn parse_initialize_response_ignores_other_responses() {
    assert!(parse_initialize_response(
        r#"{"type":"control_response","response":{"subtype":"success","request_id":"req-1","response":{"behavior":"allow"}}}"#,
        "init-1"
    )
    .is_none());
    assert!(
        parse_initialize_response(r#"{"type":"assistant","message":"hi"}"#, "init-1").is_none()
    );
}

#[test]
fn serialize_co_allow_format() {
    let s = serialize_co_allow("req-42");
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "control_response");
    assert_eq!(v["response"]["subtype"], "success");
    assert_eq!(v["response"]["request_id"], "req-42");
    assert_eq!(v["response"]["response"]["behavior"], "allow");
    assert!(v["response"]["response"].get("updatedInput").is_none());
    assert!(v["response"]["response"]
        .get("updatedPermissions")
        .is_none());
    assert!(v["response"]["response"].get("toolUseID").is_none());
}

#[test]
fn serialize_claude_allow_format() {
    let s = serialize_claude_allow("req-42", &json!({"command":"pwd"}));
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "control_response");
    assert_eq!(v["response"]["subtype"], "success");
    assert_eq!(v["response"]["request_id"], "req-42");
    assert_eq!(v["response"]["response"]["behavior"], "allow");
    assert_eq!(v["response"]["response"]["updatedInput"]["command"], "pwd");
    assert!(v["response"]["response"]
        .get("updatedPermissions")
        .is_none());
    assert!(v["response"]["response"].get("toolUseID").is_none());
}

#[test]
fn serialize_deny_format() {
    let s = serialize_deny("req-99", "User denied");
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "control_response");
    assert_eq!(v["response"]["subtype"], "success");
    assert_eq!(v["response"]["request_id"], "req-99");
    assert_eq!(v["response"]["response"]["behavior"], "deny");
    assert_eq!(v["response"]["response"]["message"], "User denied");
}

#[test]
fn serialize_host_executed_shell_result_format() {
    let result = HostExecutedShellResult {
        llm_content: "ShellCommandCompleted evidence\ncommand: df -h\nstatus: completed\nbounded_output_summary:\nFilesystem ..."
            .to_string(),
        return_display: None,
        metadata: HostExecutedShellMetadata {
            command: "df -h".to_string(),
            status: "completed".to_string(),
            exit_code: 0,
            signal: None,
            cwd: "/Users/example".to_string(),
            end_cwd: "/Users/example".to_string(),
            duration_ms: 823,
            output_ref: Some("terminal-output://block-1".to_string()),
            redaction_status: "bounded".to_string(),
            approval_id: Some("req-1".to_string()),
            tool_use_id: Some("toolu-1".to_string()),
            input_wait: None,
        },
    };
    let s = serialize_host_executed_shell_result("ctrl-1", &result);
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "control_response");
    assert_eq!(v["response"]["subtype"], "success");
    assert_eq!(v["response"]["request_id"], "ctrl-1");
    assert_eq!(v["response"]["response"]["behavior"], "host_executed_shell");
    assert_eq!(
        v["response"]["response"]["result"]["llmContent"],
        result.llm_content
    );
    assert_eq!(
        v["response"]["response"]["result"]["returnDisplay"],
        Value::Null
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["command"],
        "df -h"
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["exit_code"],
        0
    );
    assert!(v["response"]["response"]["result"]["metadata"]["signal"].is_null());
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["tool_use_id"],
        "toolu-1"
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"].get("provider_visible_complete"),
        None
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"].get("provider_visible_truncated"),
        None
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"].get("provider_visible_chars"),
        None
    );
    // #2161: no input-wait facts => the field is omitted entirely.
    assert_eq!(
        v["response"]["response"]["result"]["metadata"].get("input_wait"),
        None
    );
}

#[test]
fn serialize_host_executed_shell_result_input_wait_forms() {
    let mut result = HostExecutedShellResult {
        llm_content: "ShellCommandCompleted evidence".to_string(),
        return_display: None,
        metadata: HostExecutedShellMetadata {
            command: "bash repro.sh".to_string(),
            status: "completed".to_string(),
            exit_code: 130,
            signal: None,
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            duration_ms: 121_000,
            output_ref: None,
            redaction_status: "bounded".to_string(),
            approval_id: Some("req-1".to_string()),
            tool_use_id: Some("toolu-1".to_string()),
            input_wait: Some(HostExecutedInputWait {
                waited_secs: 120,
                interrupted: true,
            }),
        },
    };
    // Interrupted form: detected + waited + reason.
    let v: Value =
        serde_json::from_str(&serialize_host_executed_shell_result("ctrl-1", &result)).unwrap();
    let facts = &v["response"]["response"]["result"]["metadata"]["input_wait"];
    assert_eq!(facts["detected"], true);
    assert_eq!(facts["waited_secs"], 120);
    assert_eq!(facts["interrupted"], true);
    assert_eq!(facts["reason"], "input-wait-timeout");

    // Answered form: detected without interrupt carries no reason.
    result.metadata.input_wait = Some(HostExecutedInputWait {
        waited_secs: 7,
        interrupted: false,
    });
    let v: Value =
        serde_json::from_str(&serialize_host_executed_shell_result("ctrl-2", &result)).unwrap();
    let facts = &v["response"]["response"]["result"]["metadata"]["input_wait"];
    assert_eq!(facts["detected"], true);
    assert_eq!(facts["waited_secs"], 7);
    assert_eq!(facts["interrupted"], false);
    assert_eq!(facts.get("reason"), None);
}

#[test]
fn serialize_shell_evidence_result_format() {
    let result = ShellEvidenceResult {
        llm_content: "ShellEvidenceExcerpt\noutput_id: terminal-output://raw-session-a1b2/cmd-1\nexcerpt_status: available\nstdout".to_string(),
        return_display: Some("captured output".to_string()),
        metadata: ShellEvidenceMetadata {
            action: "read_output".to_string(),
            scope: None,
            limit: None,
            next_cursor: None,
            output_id: "terminal-output://raw-session-a1b2/cmd-1".to_string(),
            status: "included".to_string(),
            excerpt_status: "available".to_string(),
            reason: None,
            direction: "tail".to_string(),
            lines: 120,
            command_count: None,
            provider_visible_byte_cap: 12 * 1024,
            truncated: false,
            truncated_by_lines: false,
            truncated_by_bytes: false,
            truncation_reason: "none".to_string(),
            is_error: false,
        },
    };
    let s = serialize_shell_evidence_result("evidence-1", &result);
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "control_response");
    assert_eq!(v["response"]["subtype"], "success");
    assert_eq!(v["response"]["request_id"], "evidence-1");
    assert_eq!(v["response"]["response"]["behavior"], "shell_evidence");
    assert_ne!(
        v["response"]["response"]["behavior"],
        "shell_output_evidence"
    );
    assert_eq!(
        v["response"]["response"]["result"]["llmContent"],
        result.llm_content
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["output_id"],
        result.metadata.output_id
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["status"],
        "included"
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["action"],
        "read_output"
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["is_error"],
        false
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["truncation_reason"],
        "none"
    );
}

#[test]
fn serialize_shell_evidence_failure_format() {
    let result = ShellEvidenceResult {
        llm_content: "ShellEvidenceExcerpt\noutput_id: terminal-output://old-session/cmd-1\nexcerpt_status: unavailable\nreason: stale_session".to_string(),
        return_display: Some("stale output".to_string()),
        metadata: ShellEvidenceMetadata {
            action: "read_output".to_string(),
            scope: None,
            limit: None,
            next_cursor: None,
            output_id: "terminal-output://old-session/cmd-1".to_string(),
            status: "unavailable".to_string(),
            excerpt_status: "unavailable".to_string(),
            reason: Some("stale_session".to_string()),
            direction: "tail".to_string(),
            lines: 120,
            command_count: None,
            provider_visible_byte_cap: 12 * 1024,
            truncated: false,
            truncated_by_lines: false,
            truncated_by_bytes: false,
            truncation_reason: "none".to_string(),
            is_error: true,
        },
    };
    let s = serialize_shell_evidence_result("evidence-1", &result);
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["reason"],
        "stale_session"
    );
    assert_eq!(
        v["response"]["response"]["result"]["metadata"]["is_error"],
        true
    );
}

#[test]
fn serialize_answer_format() {
    let s = serialize_answer("ask-1", "Blue");
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "control_response");
    assert_eq!(v["response"]["subtype"], "success");
    assert_eq!(v["response"]["request_id"], "ask-1");
    assert_eq!(v["response"]["response"]["answer"], "Blue");
    assert!(v["response"]["response"].get("behavior").is_none());
}

#[test]
fn analysis_continuation_deny_only_matches_shell_tools() {
    let prompt =
        "ShellCommandCompleted evidence\nanalysis-only continuation after foreground shell handoff";

    assert!(should_deny_shell_request_for_analysis_continuation(
        prompt,
        "run_shell_command"
    ));
    assert!(!should_deny_shell_request_for_analysis_continuation(
        prompt, "Read"
    ));
    assert!(!should_deny_shell_request_for_analysis_continuation(
        "normal user prompt",
        "run_shell_command"
    ));
    assert!(!should_deny_shell_request_for_analysis_continuation(
        "normal user prompt mentioning ShellCommandCompleted evidence",
        "run_shell_command"
    ));
}

#[test]
fn analysis_continuation_shell_deny_response_preserves_request_fields() {
    let response = analysis_continuation_shell_deny_response(
        "ShellCommandCompleted evidence\nanalysis-only continuation after foreground shell handoff",
        "req-1",
        "run_shell_command",
        &json!({ "command": "df -h" }),
        "toolu-1",
    )
    .expect("deny response");

    assert_eq!(response.request_id, "req-1");
    assert_eq!(response.tool_use_id.as_deref(), Some("toolu-1"));
    assert_eq!(
        response
            .tool_input
            .as_ref()
            .and_then(|input| input.get("command"))
            .and_then(|command| command.as_str()),
        Some("df -h")
    );
    assert!(matches!(
        response.decision,
        ApprovalDecision::Deny { ref message }
            if message == ANALYSIS_ONLY_SHELL_DENY_MESSAGE
    ));
}

#[test]
fn pending_control_tool_call_drops_matching_shell_snapshot() {
    let mut pending = PendingControlProtocolToolCall::default();

    assert!(pending
        .stage_or_emit(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-1".to_string()),
            name: "shell".to_string(),
            input: "memory_pressure".to_string(),
        })
        .is_empty());

    assert!(pending.take_matching_control_shell("run-1", "toolu-1"));

    assert!(pending.flush().is_empty());
}

#[test]
fn control_staging_uses_the_canonical_provider_catalog() {
    for name in [
        "Bash",
        "shell",
        "run_shell_command",
        "tool Bash",
        "cosh_shell_evidence",
    ] {
        assert!(is_control_backed_tool_name(name), "{name}");
    }

    for name in [
        "grep",
        "edit",
        "web_fetch",
        "save_memory",
        "todo",
        "skill",
        "CustomTool",
    ] {
        assert!(!is_control_backed_tool_name(name), "{name}");
    }
}

#[test]
fn pending_control_tool_call_drops_matching_shell_evidence_snapshot_and_result() {
    let mut pending = PendingControlProtocolToolCall::default();

    assert!(pending
        .stage_or_emit(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-evidence".to_string()),
            name: "cosh_shell_evidence".to_string(),
            input: r#"{"action":"read_output"}"#.to_string(),
        })
        .is_empty());

    assert!(pending.take_matching_control_tool_call("run-1", "toolu-evidence"));
    assert!(pending
        .stage_or_emit(AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "toolu-evidence".to_string(),
            stream: "stdout".to_string(),
            text: "ShellEvidenceExcerpt".to_string(),
        })
        .is_empty());
    assert!(pending
        .stage_or_emit(AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id: "toolu-evidence".to_string(),
            status: "success".to_string(),
        })
        .is_empty());

    assert!(pending.flush().is_empty());
}

#[test]
fn pending_control_tool_call_drops_shell_evidence_result_after_late_control_request() {
    let mut pending = PendingControlProtocolToolCall::default();

    assert!(pending
        .stage_or_emit(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-evidence".to_string()),
            name: "cosh_shell_evidence".to_string(),
            input: r#"{"action":"read_output"}"#.to_string(),
        })
        .is_empty());

    let released = pending.flush_stalled(Duration::from_millis(0));
    assert!(matches!(
        &released[..],
        [AgentEvent::ToolCall {
            tool_id: Some(tool_id),
            ..
        }] if tool_id == "toolu-evidence"
    ));

    assert!(!pending.take_matching_control_tool_call("run-1", "toolu-evidence"));
    assert!(pending
        .stage_or_emit(AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "toolu-evidence".to_string(),
            stream: "stdout".to_string(),
            text: "ShellEvidenceExcerpt".to_string(),
        })
        .is_empty());
    assert!(pending
        .stage_or_emit(AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id: "toolu-evidence".to_string(),
            status: "success".to_string(),
        })
        .is_empty());
}

#[test]
fn pending_control_tool_call_consumed_tool_id_is_run_scoped() {
    let mut pending = PendingControlProtocolToolCall::default();

    assert!(!pending.take_matching_control_tool_call("run-1", "toolu-evidence"));

    let events = pending.stage_or_emit(AgentEvent::ToolCompleted {
        run_id: "run-2".to_string(),
        tool_id: "toolu-evidence".to_string(),
        status: "success".to_string(),
    });

    assert!(matches!(
        &events[..],
        [AgentEvent::ToolCompleted { run_id, tool_id, .. }]
            if run_id == "run-2" && tool_id == "toolu-evidence"
    ));
}

#[test]
fn pending_control_tool_call_consumed_tool_id_expires() {
    let mut pending = PendingControlProtocolToolCall::default();

    assert!(!pending.take_matching_control_tool_call("run-1", "toolu-evidence"));
    assert!(pending
        .stage_or_emit(AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id: "toolu-evidence".to_string(),
            status: "success".to_string(),
        })
        .is_empty());

    pending.expire_consumed_control_tool_ids_for_test();
    let events = pending.stage_or_emit(AgentEvent::ToolCompleted {
        run_id: "run-1".to_string(),
        tool_id: "toolu-evidence".to_string(),
        status: "success".to_string(),
    });

    assert!(matches!(
        &events[..],
        [AgentEvent::ToolCompleted { run_id, tool_id, .. }]
            if run_id == "run-1" && tool_id == "toolu-evidence"
    ));
}

#[test]
fn pending_control_tool_call_consumed_tool_id_releases_after_suppressed_completion() {
    let mut pending = PendingControlProtocolToolCall::default();

    assert!(!pending.take_matching_control_tool_call("run-1", "toolu-evidence"));
    assert!(pending
        .stage_or_emit(AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "toolu-evidence".to_string(),
            stream: "stdout".to_string(),
            text: "ShellEvidenceExcerpt".to_string(),
        })
        .is_empty());
    assert!(pending
        .stage_or_emit(AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id: "toolu-evidence".to_string(),
            status: "success".to_string(),
        })
        .is_empty());

    assert!(pending
        .stage_or_emit(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-evidence".to_string()),
            name: "cosh_shell_evidence".to_string(),
            input: r#"{"action":"list_commands"}"#.to_string(),
        })
        .is_empty());
    let released = pending.flush_stalled(Duration::from_millis(0));
    assert!(matches!(
        &released[..],
        [AgentEvent::ToolCall {
            run_id,
            tool_id: Some(tool_id),
            ..
        }] if run_id == "run-1" && tool_id == "toolu-evidence"
    ));
}

#[test]
fn pending_control_tool_call_releases_shell_snapshot_with_result_before_held_text() {
    let mut pending = PendingControlProtocolToolCall::default();

    assert!(pending
        .stage_or_emit(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-1".to_string()),
            name: "shell".to_string(),
            input: "memory_pressure".to_string(),
        })
        .is_empty());

    assert!(pending
        .stage_or_emit(AgentEvent::TextDelta {
            run_id: "run-1".to_string(),
            text: "final".to_string(),
        })
        .is_empty());

    let events = pending.stage_or_emit(AgentEvent::ToolOutputDelta {
        run_id: "run-1".to_string(),
        tool_id: "toolu-1".to_string(),
        stream: "stdout".to_string(),
        text: "output".to_string(),
    });

    assert!(matches!(
        &events[..],
        [
            AgentEvent::ToolCall {
                tool_id: Some(tool_id),
                ..
            },
            AgentEvent::ToolOutputDelta {
                tool_id: output_id,
                ..
            },
            AgentEvent::TextDelta { text, .. },
        ] if tool_id == "toolu-1" && output_id == "toolu-1" && text == "final"
    ));
}

#[test]
fn serialize_initialize_format() {
    let s = serialize_initialize("init-7");
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "control_request");
    assert_eq!(v["request_id"], "init-7");
    assert_eq!(v["request"]["subtype"], "initialize");
    assert!(v["request"].get("protocol_version").is_none());
}

#[test]
fn serialize_cosh_core_initialize_format() {
    let s = serialize_cosh_core_initialize("init-7");
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "control_request");
    assert_eq!(v["request_id"], "init-7");
    assert_eq!(v["request"]["subtype"], "initialize");
    assert_eq!(v["request"]["protocol_version"], CONTROL_PROTOCOL_VERSION);
}

#[test]
fn serialize_one_shot_initialize_disables_session_start() {
    let s = super::serialize_initialize_without_session_start("init-7");
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["request"]["subtype"], "initialize");
    assert_eq!(v["request"]["fire_session_start"], false);
    assert_eq!(v["request"]["protocol_version"], CONTROL_PROTOCOL_VERSION);
}

#[test]
fn serialize_user_message_format() {
    let s = serialize_user_message("hello world", Some("sess-1"));
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "user");
    assert_eq!(v["message"]["role"], "user");
    assert_eq!(v["message"]["content"], "hello world");
    assert!(v["parent_tool_use_id"].is_null());
    assert_eq!(v["session_id"], "sess-1");

    let s2 = serialize_user_message("hi", None);
    let v2: Value = serde_json::from_str(&s2).unwrap();
    assert!(v2.get("session_id").is_none());
}

#[test]
fn serialize_cosh_core_user_message_omits_missing_raw_input() {
    let with_raw = serialize_cosh_core_user_message("envelope", Some("raw"), Some("sess-1"));
    let value: Value = serde_json::from_str(&with_raw).unwrap();
    assert_eq!(value["message"]["content"], "envelope");
    assert_eq!(value["message"]["raw_user_input"], "raw");
    assert_eq!(value["session_id"], "sess-1");

    let without_raw = serialize_cosh_core_user_message("legacy", None, None);
    let value: Value = serde_json::from_str(&without_raw).unwrap();
    assert!(value["message"].get("raw_user_input").is_none());
}

#[test]
fn parse_auth_required() {
    let line = r#"{"type":"control_request","request_id":"auth-init","request":{"subtype":"auth_required","reason":"not_configured","providers":[{"id":"dashscope","label":"DashScope","description":"Use a Bailian API key","description_zh_cn":"使用百炼 API Key","builtin_base_url":"https://dashscope.example/v1","fields":[{"name":"api_key","label":"API Key","hint":"get from console","secret":true,"required":true}]},{"id":"openai_compat","label":"OpenAI","fields":[{"name":"base_url","label":"Base URL","secret":false,"required":true,"placeholder":"https://api.openai.com/v1"},{"name":"api_key","label":"Key","secret":true,"required":true}]}]}}"#;
    let req = parse_control_request(line).expect("should parse auth_required");
    match req {
        ControlRequest::AuthRequired {
            request_id,
            reason,
            error_message,
            providers,
        } => {
            assert_eq!(request_id, "auth-init");
            assert_eq!(reason, "not_configured");
            assert!(error_message.is_none());
            assert_eq!(providers.len(), 2);
            assert_eq!(providers[0].id, "dashscope");
            assert_eq!(providers[0].label, "DashScope");
            assert_eq!(
                providers[0].description.as_deref(),
                Some("Use a Bailian API key")
            );
            assert_eq!(
                providers[0].description_zh_cn.as_deref(),
                Some("使用百炼 API Key")
            );
            assert_eq!(
                providers[0].builtin_base_url.as_deref(),
                Some("https://dashscope.example/v1")
            );
            assert_eq!(providers[0].fields.len(), 1);
            assert_eq!(providers[0].fields[0].name, "api_key");
            assert!(providers[0].fields[0].secret);
            assert!(providers[0].fields[0].required);
            assert_eq!(
                providers[0].fields[0].hint.as_deref(),
                Some("get from console")
            );
            assert_eq!(providers[1].id, "openai_compat");
            assert!(providers[1].description.is_none());
            assert!(providers[1].description_zh_cn.is_none());
            assert!(providers[1].builtin_base_url.is_none());
            assert_eq!(providers[1].fields.len(), 2);
            assert_eq!(
                providers[1].fields[0].placeholder.as_deref(),
                Some("https://api.openai.com/v1")
            );
        }
        _ => panic!("expected AuthRequired"),
    }
}

#[test]
fn serialize_auth_response_format() {
    let mut values = HashMap::new();
    values.insert("api_key".to_string(), "sk-test".to_string());
    let s = serialize_auth_response("auth-init", "qwen-prod", Some("dashscope"), &values, true);
    let v: Value = serde_json::from_str(&s).unwrap();
    assert_eq!(v["type"], "control_response");
    assert_eq!(v["response"]["subtype"], "success");
    assert_eq!(v["response"]["request_id"], "auth-init");
    assert_eq!(v["response"]["response"]["provider_id"], "qwen-prod");
    assert_eq!(v["response"]["response"]["provider_type"], "dashscope");
    assert_eq!(v["response"]["response"]["values"]["api_key"], "sk-test");
    assert_eq!(v["response"]["response"]["persist"], true);
}
