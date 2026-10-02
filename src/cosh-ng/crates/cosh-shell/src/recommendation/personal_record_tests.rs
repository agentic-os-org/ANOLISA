use crate::types::{
    AgentContextBinding, AgentEvent, AgentMode, AgentRequest, CommandBlock, CommandOrigin,
    CommandStatus, OutputRefs,
};

use super::personal_model::{ActivityOutcome, ActivityPayload, ToolCategory};
use super::personal_record::{agent_request_record, agent_run_record, shell_command_record};

fn block(origin: CommandOrigin) -> CommandBlock {
    CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "real-session-must-not-persist".to_string(),
        command: "kubectl logs payment-api -n production".to_string(),
        origin,
        cwd: "/tmp/payment".to_string(),
        end_cwd: "/tmp/payment".to_string(),
        started_at_ms: 10,
        ended_at_ms: 20,
        duration_ms: 10,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    }
}

fn request() -> AgentRequest {
    AgentRequest {
        id: "request-1".to_string(),
        session_id: "real-session-must-not-persist".to_string(),
        command_block: block(CommandOrigin::UserInteractive),
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some("分析 payment-api 的内存压力 token=request-secret".to_string()),
        findings: Vec::new(),
        mode: AgentMode::AnalysisOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: Some("memory-diagnosis".to_string()),
    }
}

#[test]
fn shell_source_gate_records_only_user_owned_origins() {
    for origin in [
        CommandOrigin::UserInteractive,
        CommandOrigin::UserSendToShell,
        CommandOrigin::UserAnalysisAction,
    ] {
        assert!(shell_command_record(
            &block(origin),
            "act-1",
            "session-opaque",
            "fingerprint-1",
            Default::default(),
            None,
        )
        .is_some());
    }
    for origin in [
        CommandOrigin::AgentHandoff,
        CommandOrigin::ProviderTool,
        CommandOrigin::ShellInternal,
        CommandOrigin::Unknown,
    ] {
        assert!(shell_command_record(
            &block(origin),
            "act-1",
            "session-opaque",
            "fingerprint-1",
            Default::default(),
            None,
        )
        .is_none());
    }
}

#[test]
fn agent_request_keeps_sanitized_text_and_opaque_session_only() {
    let record = agent_request_record(
        &request(),
        AgentContextBinding::FreeForm,
        "act-request",
        "session-opaque",
        "fingerprint-request",
        "intent-opaque",
        Default::default(),
        None,
    )
    .expect("request activity");

    assert_eq!(record.session_scope_id.as_deref(), Some("session-opaque"));
    let json = serde_json::to_string(&record).unwrap();
    assert!(!json.contains("real-session-must-not-persist"));
    assert!(!json.contains("request-secret"));
    assert!(json.contains("payment-api"));
    let ActivityPayload::AgentRequest {
        system_recommended_skill,
        ..
    } = record.payload
    else {
        panic!("agent request payload");
    };
    assert_eq!(
        system_recommended_skill.as_deref(),
        Some("memory-diagnosis")
    );
}

/// The shell secret gate flagged this input sensitive (#2138): the
/// personalization sink must whole-field redact instead of trusting the
/// sanitizer regexes — `sk-fbaa6` is below the opaque-token minimum
/// length and would survive `sanitize_agent_request` verbatim.
#[test]
fn agent_request_with_sensitive_hint_redacts_whole_input_field() {
    let mut sensitive_request = request();
    sensitive_request.user_input =
        Some("帮我安装下openclaw,模型使用qwen3.8-max,API Key: sk-fbaa6".to_string());
    crate::types::mark_request_sensitive_input(&mut sensitive_request);

    let record = agent_request_record(
        &sensitive_request,
        AgentContextBinding::FreeForm,
        "act-request",
        "session-opaque",
        "fingerprint-request",
        "intent-opaque",
        Default::default(),
        None,
    )
    .expect("request activity");

    let json = serde_json::to_string(&record).unwrap();
    assert!(!json.contains("sk-fbaa6"), "{json}");
    let ActivityPayload::AgentRequest { text, .. } = record.payload else {
        panic!("agent request payload");
    };
    assert_eq!(text, "<redacted>");
}

#[test]
fn control_continuations_are_not_new_user_intents() {
    for binding in [
        AgentContextBinding::ControlProtocolEvidence,
        AgentContextBinding::ShellHandoffContinuation,
    ] {
        assert!(agent_request_record(
            &request(),
            binding,
            "act-request",
            "session-opaque",
            "fingerprint-request",
            "intent-opaque",
            Default::default(),
            None,
        )
        .is_none());
    }
}

#[test]
fn agent_run_uses_only_broad_tool_categories_and_terminal_outcome() {
    let events = vec![
        AgentEvent::TextDelta {
            run_id: "run-1".to_string(),
            text: "response-secret-must-not-be-read".to_string(),
        },
        AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("tool-1".to_string()),
            name: "read_file".to_string(),
            input: "tool-input-secret-must-not-be-read".to_string(),
        },
        AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("tool-2".to_string()),
            name: "bash".to_string(),
            input: "another-secret".to_string(),
        },
        AgentEvent::AgentCompleted {
            run_id: "run-1".to_string(),
            summary: "summary-secret-must-not-be-read".to_string(),
        },
    ];

    let record = agent_run_record(
        "act-request",
        &events,
        "act-run",
        "session-opaque",
        "fingerprint-run",
        Default::default(),
    )
    .expect("run activity");
    let json = serde_json::to_string(&record).unwrap();
    for secret in ["response-secret", "tool-input-secret", "summary-secret"] {
        assert!(!json.contains(secret), "{json}");
    }
    let ActivityPayload::AgentRun {
        tool_categories,
        outcome,
        ..
    } = record.payload
    else {
        panic!("agent run payload");
    };
    assert_eq!(
        tool_categories,
        vec![ToolCategory::FilesystemRead, ToolCategory::Shell]
    );
    assert_eq!(outcome, ActivityOutcome::Success);
}
