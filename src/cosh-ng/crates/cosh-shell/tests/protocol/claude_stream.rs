use std::os::unix::fs::PermissionsExt;

use cosh_shell::adapter::{AgentAdapter, ClaudeCodeAdapter};
use cosh_shell::types::{
    AgentEvent, AgentMode, AgentRequest, CommandBlock, CommandStatus, CoshApprovalMode, OutputRefs,
    QuestionSelectionMode,
};

fn make_request(user_input: Option<&str>) -> AgentRequest {
    AgentRequest {
        id: "agent-request-cmd-1".to_string(),
        session_id: "session-1".to_string(),
        command_block: CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: "missing-command".to_string(),
            origin: Default::default(),
            cwd: "/work".to_string(),
            end_cwd: "/work".to_string(),
            started_at_ms: 1000,
            ended_at_ms: 1037,
            duration_ms: 37,
            exit_code: 127,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: Some("terminal://session-1/cmd-1".to_string()),
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: user_input.map(str::to_string),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

#[test]
fn claude_code_adapter_streams_question_options_from_input_json_delta() {
    let request = make_request(Some("问我喜欢什么颜色"));

    let script = std::env::temp_dir().join(format!(
        "cosh-shell-claude-stream-question-{}.sh",
        std::process::id()
    ));
    std::fs::write(
        &script,
        r#"#!/bin/sh
printf '%s\n' '{"type":"system","subtype":"init","session_id":"session-stream"}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{}}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{\"questions\":[{\"question\":\"你喜欢什么颜色？\",\"header\":\"颜色\",\"options\":[{\"label\":\"白色\"},{\"label\":\"黑色\"},{\"label\":\"蓝色\"}],\"multiSelect\":false}]}"}}}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_stop","index":0}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"session_id":"session-stream"}'
"#,
    )
    .expect("write fake claude script");
    let mut permissions = std::fs::metadata(&script)
        .expect("fake claude script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("fake claude script permissions");

    let mut adapter = ClaudeCodeAdapter::default().with_model_call(true);
    adapter.program = script.to_string_lossy().to_string();
    let mut streamed = Vec::new();
    let result = adapter.run_stream(&request, &mut |event| {
        streamed.push(event);
        Ok(())
    });
    let _ = std::fs::remove_file(&script);
    result.expect("stream fake claude adapter");

    assert!(streamed.iter().any(|event| matches!(
        event,
        AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        } if question == "你喜欢什么颜色？"
            && options == &vec![
                "白色".to_string(),
                "黑色".to_string(),
                "蓝色".to_string(),
            ]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    )));
    assert!(streamed.iter().any(|event| matches!(
        event,
        AgentEvent::AgentCompleted { summary, .. }
            if summary.contains("analysis completed")
    )));
}

#[test]
fn claude_code_adapter_reads_question_options_from_permission_request() {
    let request = make_request(Some("问我喜欢什么颜色"));

    let script = std::env::temp_dir().join(format!(
        "cosh-shell-claude-permission-question-{}.sh",
        std::process::id()
    ));
    std::fs::write(
        &script,
        r#"#!/bin/sh
printf '%s\n' '{"type":"system","subtype":"init","session_id":"session-permission"}'
printf '%s\n' '{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tool-ask","name":"AskUserQuestion","input":{}}]}}'
printf '%s\n' '{"event":"permission_request","toolName":"AskUserQuestion","input":{"questions":[{"question":"你喜欢什么颜色？","header":"颜色","options":[{"label":"白色"},{"label":"黑色"},{"label":"蓝色"}],"multiSelect":false}]},"permissionLevel":null}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"session_id":"session-permission"}'
"#,
    )
    .expect("write fake claude script");
    let mut permissions = std::fs::metadata(&script)
        .expect("fake claude script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("fake claude script permissions");

    let mut adapter = ClaudeCodeAdapter::default().with_model_call(true);
    adapter.program = script.to_string_lossy().to_string();
    let mut streamed = Vec::new();
    let result = adapter.run_stream(&request, &mut |event| {
        streamed.push(event);
        Ok(())
    });
    let _ = std::fs::remove_file(&script);
    result.expect("stream fake claude adapter");

    assert!(streamed.iter().any(|event| matches!(
        event,
        AgentEvent::UserQuestion {
            question,
            options,
            allow_free_text,
            selection_mode,
            ..
        } if question == "你喜欢什么颜色？"
            && options == &vec![
                "白色".to_string(),
                "黑色".to_string(),
                "蓝色".to_string(),
            ]
            && *allow_free_text
            && *selection_mode == QuestionSelectionMode::Single
    )));
    assert_eq!(
        streamed
            .iter()
            .filter(|event| matches!(event, AgentEvent::UserQuestion { .. }))
            .count(),
        1
    );
}

#[test]
fn claude_code_adapter_reuses_stream_session_id_on_next_invocation() {
    let request = make_request(None);

    let script = std::env::temp_dir().join(format!(
        "cosh-shell-claude-resume-session-{}.sh",
        std::process::id()
    ));
    std::fs::write(
        &script,
        r#"#!/bin/sh
printf '%s\n' '{"type":"system","subtype":"init","session_id":"session-stream"}'
printf '%s\n' '{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"first response"}}}'
printf '%s\n' '{"type":"result","subtype":"success","is_error":false,"session_id":"session-stream"}'
"#,
    )
    .expect("write fake claude script");
    let mut permissions = std::fs::metadata(&script)
        .expect("fake claude script metadata")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).expect("fake claude script permissions");

    let mut adapter = ClaudeCodeAdapter::default().with_model_call(true);
    adapter.program = script.to_string_lossy().to_string();
    let mut streamed = Vec::new();
    let result = adapter.run_stream(&request, &mut |event| {
        streamed.push(event);
        Ok(())
    });
    let _ = std::fs::remove_file(&script);
    result.expect("stream fake claude adapter");

    let next_invocation = adapter.prepare_invocation(&request, CoshApprovalMode::default());
    let resume_at = next_invocation
        .args
        .iter()
        .position(|arg| arg == "--resume")
        .expect("resume flag");
    assert_eq!(
        next_invocation.args.get(resume_at + 1).map(String::as_str),
        Some("session-stream")
    );
}
