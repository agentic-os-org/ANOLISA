use super::runtime::*;
use super::runtime_render::tool_invocation_cards_for_test;
use crate::runtime::prelude::*;
use std::os::unix::fs::PermissionsExt;

fn governed(event: AgentEvent) -> GovernedEvent {
    GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event,
        reason: "test".to_string(),
        display_text: "test".to_string(),
        auto_execute: false,
    }
}

#[test]
fn activity_tool_output_summary_uses_state_language() {
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "tool-1".to_string(),
            stream: "stdout".to_string(),
            text: "line 1\nline 2".to_string(),
        })],
    );

    assert_eq!(ids, vec!["out-1"]);
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == "out-1")
        .expect("activity row");
    assert_eq!(row.summary, "stdout 已捕获");
    assert!(!row.summary.contains("out-1"));
    assert!(row.detail.contains("stream: stdout"));

    let mut output = Vec::new();
    render_activity_details_by_id(&state, "out-1", &mut output)
        .expect("details result")
        .expect("render details");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("活动详情 out-1"), "{output}");
    assert!(output.contains("运行: run-1"), "{output}");
    assert!(output.contains("详情:"), "{output}");
    assert!(output.contains("stream: stdout"), "{output}");
}

#[test]
fn activity_tool_output_details_hide_internal_output_ref_path() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-activity-details-hide-ref-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut state = InlineState::with_raw_session_dir(&dir);
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "tool-1".to_string(),
            stream: "stdout".to_string(),
            text: "secret-ish\n".to_string(),
        })],
    );

    assert_eq!(ids, vec!["out-1"]);
    let output_ref = dir.join("agent-output-refs/out-1.txt");
    assert!(output_ref.exists(), "output ref should still be captured");

    let mut output = Vec::new();
    render_activity_details_by_id(&state, "out-1", &mut output)
        .expect("details result")
        .expect("render details");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("capture_status: captured"), "{output}");
    assert!(output.contains("output_ref: <hidden>"), "{output}");
    assert!(!output.contains(output_ref.to_str().unwrap()), "{output}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn activity_tool_output_details_show_internal_output_ref_in_debug() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-activity-details-debug-ref-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut state = InlineState {
        debug: true,
        ..InlineState::with_raw_session_dir(&dir)
    };
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "tool-1".to_string(),
            stream: "stdout".to_string(),
            text: "debug-visible\n".to_string(),
        })],
    );

    assert_eq!(ids, vec!["out-1"]);
    let output_ref = dir.join("agent-output-refs/out-1.txt");
    assert!(output_ref.exists(), "output ref should be captured");

    let mut output = Vec::new();
    render_activity_details_by_id(&state, "out-1", &mut output)
        .expect("details result")
        .expect("render details");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("debug_output_ref:"), "{output}");
    assert!(output.contains("out-1.txt"), "{output}");
    assert!(output.contains("output_ref: <hidden>"), "{output}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn output_only_tool_invocation_renders_result_card_not_call_card() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "toolu-custom".to_string(),
            stream: "stdout".to_string(),
            text: "line 1\nline 2\n".to_string(),
        })],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("toolu-custom captured"), "{output}");
    assert!(output.contains("line 1"), "{output}");
    assert!(output.contains("stdout: 2 lines"), "{output}");
    assert!(!output.contains("toolu-custom called"), "{output}");
    assert!(!output.contains("[Details]"), "{output}");
}

#[test]
fn output_only_tool_invocation_refreshes_result_across_deltas() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-custom".to_string(),
                stream: "stdout".to_string(),
                text: "line 1\n".to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-custom".to_string(),
                stream: "stdout".to_string(),
                text: "line 2\n".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("toolu-custom captured"), "{output}");
    assert!(output.contains("stdout: 2 lines"), "{output}");
}

#[test]
fn non_shell_provider_tool_call_without_result_does_not_render_activity_row() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: None,
            name: "Read".to_string(),
            input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
        })],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.trim().is_empty(), "{output}");
    assert_eq!(state.activity.tool_invocations.len(), 1);
    assert_eq!(
        state.activity.tool_invocations[0].phase,
        ToolInvocationPhase::Call
    );
}

#[test]
fn zh_tool_result_card_uses_localized_labels() {
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-read".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                stream: "stdout".to_string(),
                text: "line\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("读取 已完成"), "{output}");
    assert!(output.contains("Cargo.toml"), "{output}");
    assert!(output.contains("返回 1 行"), "{output}");
    assert!(!output.contains("read-only operation"), "{output}");
    assert!(!output.contains("[Details]"), "{output}");
}

#[test]
fn zh_skill_list_result_card_localizes_action_and_empty_result() {
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-skill-list".to_string()),
                name: "skill".to_string(),
                input: r#"{"action":"list"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-skill-list".to_string(),
                stream: "stdout".to_string(),
                text: "No skills found.\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-skill-list".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("技能 已完成"), "{output}");
    assert!(output.contains("技能列表"), "{output}");
    assert!(output.contains("未找到技能"), "{output}");
    assert!(output.contains("技能: 0"), "{output}");
    assert!(!output.contains("skill 已完成"), "{output}");
    assert!(!output.contains("action: list"), "{output}");
    assert!(!output.contains("No skills found."), "{output}");
    assert!(!output.contains("stdout: 1 行"), "{output}");
}

#[test]
fn zh_tool_result_cards_localize_common_summaries_and_metrics() {
    let cases = [
        (
            "write_file",
            r#"{"file_path":"/tmp/report.md","content":"new body"}"#,
            None,
            "写入 已完成",
            "/tmp/report.md",
            "写入完成: 新文件",
            Vec::<&str>::new(),
        ),
        (
            "Glob",
            r#"{"pattern":"**/*.rs"}"#,
            Some(r#"{"files":["a.rs","b.rs"]}"#),
            "查找文件 已完成",
            "**/*.rs",
            "2个文件",
            vec!["文件: 2"],
        ),
        (
            "save_memory",
            r#"{"fact":"remember display rule"}"#,
            None,
            "记忆 已完成",
            "remember display rule",
            "记忆已更新",
            Vec::<&str>::new(),
        ),
    ];

    for (name, input, output, title, primary, result, metrics) in cases {
        let mut state = InlineState {
            language: Language::ZhCn,
            ..InlineState::default()
        };
        let tool_id = format!("{name}-1");
        let mut events = vec![governed(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some(tool_id.clone()),
            name: name.to_string(),
            input: input.to_string(),
        })];
        if let Some(output) = output {
            events.push(governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: tool_id.clone(),
                stream: "stdout".to_string(),
                text: output.to_string(),
            }));
        }
        events.push(governed(AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id,
            status: "success".to_string(),
        }));

        let ids = record_activity_rows(&mut state, &events);
        let cards = tool_invocation_cards_for_test(&state, &ids);

        assert_eq!(cards.len(), 1, "{name}");
        assert_eq!(cards[0].title, title, "{name}");
        assert_eq!(cards[0].primary, primary, "{name}");
        assert_eq!(cards[0].result, result, "{name}");
        assert_eq!(cards[0].metrics, metrics, "{name}");
        assert!(
            !cards[0].result.contains("new file")
                && !cards[0].result.contains("updated")
                && !cards[0]
                    .metrics
                    .iter()
                    .any(|metric| metric.starts_with("files:")),
            "{name}: {:?}",
            cards[0]
        );
    }
}

#[test]
fn completed_tool_invocation_replaces_call_card_with_result_card() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-read".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                stream: "stdout".to_string(),
                text: "line 1\nline 2\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("Read completed"), "{output}");
    assert!(output.contains("Cargo.toml"), "{output}");
    assert!(output.contains("2 lines returned"), "{output}");
    assert!(output.contains("stdout: 2 lines"), "{output}");
    assert!(!output.contains("Read called"), "{output}");
    assert!(!output.contains("Tool output"), "{output}");
    assert!(!output.contains("[Details]"), "{output}");
}

#[test]
fn invocation_id_uses_provider_tool_id_for_call_output_and_completion() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let _ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("provider-read-1".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "provider-read-1".to_string(),
                stream: "stdout".to_string(),
                text: "line 1\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "provider-read-1".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    assert_eq!(state.activity.tool_invocations.len(), 1);
    let record = &state.activity.tool_invocations[0];
    assert_eq!(record.invocation_id, "provider-read-1");
    assert_eq!(record.phase, ToolInvocationPhase::Result);
    assert_eq!(record.output.stdout_lines, 1);
}

#[test]
fn invocation_id_uses_control_tool_use_id_for_permission_requests() {
    let mut state = InlineState {
        language: Language::EnUs,
        approval_mode: CoshApprovalMode::Recommend,
        ..InlineState::default()
    };
    let _ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolPermissionRequest {
            run_id: "run-1".to_string(),
            request_id: "ctrl-read-1".to_string(),
            tool_name: "Read".to_string(),
            tool_input: serde_json::json!({ "file_path": "Cargo.toml" }),
            tool_use_id: "toolu-read-1".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        })],
    );

    assert_eq!(state.activity.tool_invocations.len(), 1);
    let record = &state.activity.tool_invocations[0];
    assert_eq!(record.invocation_id, "toolu-read-1");
    assert_ne!(record.invocation_id, "ctrl-read-1");
    assert_eq!(record.lifecycle, "requested");
}

#[test]
fn invocation_id_falls_back_to_request_id_when_control_tool_use_id_is_missing() {
    let mut state = InlineState {
        language: Language::EnUs,
        approval_mode: CoshApprovalMode::Recommend,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolPermissionRequest {
            run_id: "run-1".to_string(),
            request_id: "ctrl-read-1".to_string(),
            tool_name: "Read".to_string(),
            tool_input: serde_json::json!({ "file_path": "Cargo.toml" }),
            tool_use_id: String::new(),
            hook_requires_approval: false,
            audit_ref: None,
        })],
    );

    assert_eq!(state.activity.tool_invocations.len(), 1);
    let record = &state.activity.tool_invocations[0];
    assert_eq!(record.invocation_id, "ctrl-read-1");
    assert_eq!(record.lifecycle, "requested");
    assert_eq!(ids.len(), 1);
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == ids[0])
        .expect("activity row");
    assert_eq!(row.subject, "ctrl-read-1");
}

#[test]
fn invocation_id_uses_run_id_event_index_for_missing_provider_tool_id() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let _ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: None,
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: None,
                name: "grep".to_string(),
                input: r#"{"path":"src","query":"needle"}"#.to_string(),
            }),
        ],
    );

    assert_eq!(state.activity.tool_invocations.len(), 2);
    assert_eq!(
        state.activity.tool_invocations[0].invocation_id,
        "run-1:event-0"
    );
    assert_eq!(
        state.activity.tool_invocations[1].invocation_id,
        "run-1:event-1"
    );
}

#[test]
fn completed_tool_without_provider_call_id_replaces_matching_call_card() {
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: None,
                name: "grep".to_string(),
                input: r#"{"path":"src/cosh-ng","query":"sysom"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "grep".to_string(),
                stream: "stdout".to_string(),
                text: "match\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "grep".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("搜索 已完成"), "{output}");
    assert!(output.contains("命中 1 处"), "{output}");
    assert!(!output.contains("grep 已调用"), "{output}");
    assert!(!output.contains("正在思考"), "{output}");

    assert_eq!(state.activity.tool_invocations.len(), 1);
    let record = &state.activity.tool_invocations[0];
    assert_eq!(record.invocation_id, "run-1:event-0");
    assert_eq!(record.phase, ToolInvocationPhase::Result);
    assert_eq!(record.output.stdout_lines, 1);
}

#[test]
fn completed_tool_result_owner_suppresses_legacy_call_row_later() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: None,
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "Read".to_string(),
                stream: "stdout".to_string(),
                text: "line\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "Read".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &[ids[0].clone()], &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("Read completed"), "{output}");
    assert!(!output.contains("Read called"), "{output}");
    assert!(!output.contains("Activity"), "{output}");
}

#[test]
fn ambiguous_same_name_calls_without_provider_id_use_independent_result_fallback() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: None,
                name: "Read".to_string(),
                input: r#"{"file_path":"first.txt"}"#.to_string(),
            }),
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: None,
                name: "Read".to_string(),
                input: r#"{"file_path":"second.txt"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "Read".to_string(),
                stream: "stdout".to_string(),
                text: "ambiguous result\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "Read".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    assert_eq!(state.activity.tool_invocations.len(), 3);
    assert_eq!(
        state.activity.tool_invocations[0].invocation_id,
        "run-1:event-0"
    );
    assert_eq!(
        state.activity.tool_invocations[1].invocation_id,
        "run-1:event-1"
    );
    assert_eq!(
        state.activity.tool_invocations[2].invocation_id,
        "run-1:event-2"
    );
    assert_eq!(
        state.activity.tool_invocations[0].phase,
        ToolInvocationPhase::Call
    );
    assert_eq!(
        state.activity.tool_invocations[1].phase,
        ToolInvocationPhase::Call
    );
    assert_eq!(
        state.activity.tool_invocations[2].phase,
        ToolInvocationPhase::Result
    );
    assert_eq!(state.activity.tool_invocations[0].output.stdout_lines, 0);
    assert_eq!(state.activity.tool_invocations[1].output.stdout_lines, 0);
    assert_eq!(state.activity.tool_invocations[2].output.stdout_lines, 1);

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("Read completed"), "{output}");
    assert!(output.contains("ambiguous result"), "{output}");
    assert!(!output.contains("Read called: first.txt"), "{output}");
    assert!(!output.contains("Read called: second.txt"), "{output}");
}

#[test]
fn shell_evidence_list_result_uses_evidence_receipt_language() {
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: None,
                name: "cosh_shell_evidence".to_string(),
                input: r#"{"action":"list_commands","limit":20}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "cosh_shell_evidence".to_string(),
                stream: "stdout".to_string(),
                text: "line\n".repeat(22),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "cosh_shell_evidence".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("Shell 证据 已完成"), "{output}");
    assert!(output.contains("命令历史已交给 Agent"), "{output}");
    assert!(!output.contains("Shell 证据 已调用"), "{output}");
    assert!(!output.contains("list_commands"), "{output}");
    assert!(!output.contains("证据元数据"), "{output}");
    assert!(!output.contains("output ref:"), "{output}");
    assert!(!output.contains("stdout: 22 行"), "{output}");
}

#[test]
fn shell_evidence_failure_hides_protocol_header_and_output_ref() {
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: None,
                name: "cosh_shell_evidence".to_string(),
                input: r#"{"action":"read_output","output_id":"terminal-output://s/cmd-2"}"#
                    .to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "cosh_shell_evidence".to_string(),
                stream: "stderr".to_string(),
                text: "ShellEvidenceExcerpt\nreason: not_in_current_ledger\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "cosh_shell_evidence".to_string(),
                status: "failed".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("Shell 证据 失败"), "{output}");
    assert!(output.contains("Shell 证据不可用"), "{output}");
    assert!(!output.contains("ShellEvidenceExcerpt"), "{output}");
    assert!(!output.contains("read_output"), "{output}");
    assert!(!output.contains("terminal-output://"), "{output}");
    assert!(!output.contains("output ref:"), "{output}");
    assert!(!output.contains("stderr:"), "{output}");
}

#[test]
fn result_density_read_only_success_is_receipt() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("read-1".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "read-1".to_string(),
                stream: "stdout".to_string(),
                text: "line 1\nline 2\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "read-1".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let cards = tool_invocation_cards_for_test(&state, &ids);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].density, ToolInvocationDensity::Receipt);
    assert_eq!(cards[0].status, "success");
}

#[test]
fn file_search_result_prefers_match_and_file_counts() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("grep-1".to_string()),
                name: "FileSearch".to_string(),
                input: r#"{"query":"needle","path":"src"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "grep-1".to_string(),
                stream: "stdout".to_string(),
                text: "src/a.rs:10:needle\nsrc/b.rs:20:needle\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "grep-1".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let cards = tool_invocation_cards_for_test(&state, &ids);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].title, "Search completed");
    assert_eq!(cards[0].primary, "\"needle\" in src");
    assert_eq!(cards[0].result, "2 matches in 2 files");
    assert_eq!(cards[0].metrics, vec!["matches: 2", "files: 2"]);
}

#[test]
fn structured_tool_results_extract_user_facing_metrics() {
    let cases = [
        (
            "Glob",
            r#"{"pattern":"**/*.rs"}"#,
            r#"["src/a.rs","src/b.rs"]"#,
            "Find files completed",
            "**/*.rs",
            "2 files",
            vec!["files: 2"],
        ),
        (
            "LS",
            r#"{"path":"src"}"#,
            r#"{"entries":[{"name":"bin","type":"directory"},{"name":"main.rs","type":"file"}]}"#,
            "List directory completed",
            "src",
            "2 entries",
            vec!["files: 1", "dirs: 1"],
        ),
        (
            "LSP",
            r#"{"operation":"references","filePath":"src/main.rs","line":7}"#,
            r#"{"locations":[{"uri":"file:///src/main.rs"}]}"#,
            "Search completed",
            "src/main.rs:7",
            "1 location",
            vec!["locations: 1"],
        ),
        (
            "WebSearch",
            r#"{"query":"rust async"}"#,
            r#"{"results":[{"title":"Rust async book"},{"title":"Tokio"}]}"#,
            "Web search completed",
            "\"rust async\"",
            "2 results",
            vec!["top: Rust async book"],
        ),
        (
            "WebFetch",
            r#"{"url":"https://example.com"}"#,
            r#"{"status_code":200,"title":"Example Domain","content":"hello world"}"#,
            "Web fetch completed",
            "https://example.com",
            "page fetched",
            vec!["status: 200", "title: Example Domain", "content: 11 chars"],
        ),
    ];

    for (name, input, output, title, primary, result, metrics) in cases {
        let mut state = InlineState {
            language: Language::EnUs,
            ..InlineState::default()
        };
        let ids = record_activity_rows(
            &mut state,
            &[
                governed(AgentEvent::ToolCall {
                    run_id: "run-1".to_string(),
                    tool_id: Some("tool-1".to_string()),
                    name: name.to_string(),
                    input: input.to_string(),
                }),
                governed(AgentEvent::ToolOutputDelta {
                    run_id: "run-1".to_string(),
                    tool_id: "tool-1".to_string(),
                    stream: "stdout".to_string(),
                    text: output.to_string(),
                }),
                governed(AgentEvent::ToolCompleted {
                    run_id: "run-1".to_string(),
                    tool_id: "tool-1".to_string(),
                    status: "success".to_string(),
                }),
            ],
        );

        let cards = tool_invocation_cards_for_test(&state, &ids);
        assert_eq!(cards.len(), 1, "{name}");
        assert_eq!(cards[0].title, title, "{name}");
        assert_eq!(cards[0].primary, primary, "{name}");
        assert_eq!(cards[0].result, result, "{name}");
        assert_eq!(cards[0].metrics, metrics, "{name}");
    }
}

#[test]
fn context_mutation_result_cards_keep_specific_receipts() {
    let cases = [
        (
            "save_memory",
            r#"{"fact":"remember display rule"}"#,
            "Memory completed",
            "remember display rule",
            "memory saved",
        ),
        (
            "TodoWrite",
            r#"{"task_id":"todo-1"}"#,
            "Todo completed",
            "todo-1",
            "todo updated",
        ),
        (
            "TaskCreate",
            r#"{"title":"follow up"}"#,
            "Task completed",
            "follow up",
            "task created",
        ),
        (
            "CronDelete",
            r#"{"cron_id":"cron-1"}"#,
            "Cron completed",
            "cron-1",
            "cron deleted",
        ),
        (
            "ScheduleWakeup",
            r#"{"time":"2026-06-27T09:00:00+08:00"}"#,
            "Wakeup completed",
            "2026-06-27T09:00:00+08:00",
            "wakeup scheduled",
        ),
    ];

    for (name, input, title, primary, result) in cases {
        let mut state = InlineState {
            language: Language::EnUs,
            ..InlineState::default()
        };
        let ids = record_activity_rows(
            &mut state,
            &[
                governed(AgentEvent::ToolCall {
                    run_id: "run-1".to_string(),
                    tool_id: Some("tool-1".to_string()),
                    name: name.to_string(),
                    input: input.to_string(),
                }),
                governed(AgentEvent::ToolCompleted {
                    run_id: "run-1".to_string(),
                    tool_id: "tool-1".to_string(),
                    status: "success".to_string(),
                }),
            ],
        );

        let cards = tool_invocation_cards_for_test(&state, &ids);
        assert_eq!(cards.len(), 1, "{name}");
        assert_eq!(cards[0].title, title, "{name}");
        assert_eq!(cards[0].primary, primary, "{name}");
        assert_eq!(cards[0].result, result, "{name}");
    }
}

#[test]
fn result_density_write_success_is_summary() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("write-1".to_string()),
                name: "write_file".to_string(),
                input: r#"{"file_path":"/tmp/report.md","content":"new body"}"#.to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "write-1".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let cards = tool_invocation_cards_for_test(&state, &ids);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].density, ToolInvocationDensity::Summary);
    assert_eq!(cards[0].status, "success");
    assert_eq!(cards[0].primary, "/tmp/report.md");
    assert_eq!(cards[0].result, "write completed: new file");
}

#[test]
fn edit_success_result_shows_change_summary() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("edit-1".to_string()),
                name: "NotebookEdit".to_string(),
                input: r#"{"file_path":"notes.ipynb","old_string":"before value","new_string":"after value"}"#.to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "edit-1".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let cards = tool_invocation_cards_for_test(&state, &ids);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].density, ToolInvocationDensity::Summary);
    assert_eq!(cards[0].primary, "notes.ipynb");
    assert_eq!(
        cards[0].result,
        "edit completed: before value -> after value"
    );
}

#[test]
fn result_density_failure_is_diagnostic() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("read-1".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"missing.txt"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "read-1".to_string(),
                stream: "stderr".to_string(),
                text: "No such file\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "read-1".to_string(),
                status: "failed".to_string(),
            }),
        ],
    );

    let cards = tool_invocation_cards_for_test(&state, &ids);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].density, ToolInvocationDensity::Diagnostic);
    assert_eq!(cards[0].status, "failed");
    assert_eq!(cards[0].result, "No such file");
}

#[test]
fn result_density_interrupted_denied_and_cancelled_are_diagnostic() {
    for status in ["interrupted", "denied", "cancelled"] {
        let mut state = InlineState {
            language: Language::EnUs,
            ..InlineState::default()
        };
        let ids = record_activity_rows(
            &mut state,
            &[
                governed(AgentEvent::ToolCall {
                    run_id: format!("run-{status}"),
                    tool_id: Some(format!("read-{status}")),
                    name: "Read".to_string(),
                    input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
                }),
                governed(AgentEvent::ToolCompleted {
                    run_id: format!("run-{status}"),
                    tool_id: format!("read-{status}"),
                    status: status.to_string(),
                }),
            ],
        );

        let cards = tool_invocation_cards_for_test(&state, &ids);
        assert_eq!(cards.len(), 1, "{status}");
        assert_eq!(
            cards[0].density,
            ToolInvocationDensity::Diagnostic,
            "{status}"
        );
        assert_eq!(cards[0].status, status);
        assert_eq!(cards[0].result, status);
    }
}

#[test]
fn shell_success_with_transcript_does_not_duplicate_result_card() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("shell-1".to_string()),
                name: "Bash".to_string(),
                input: r#"{"command":"printf hi"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "shell-1".to_string(),
                stream: "stdout".to_string(),
                text: "hi\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "shell-1".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let cards = tool_invocation_cards_for_test(&state, &ids);
    assert!(cards.is_empty(), "{cards:?}");
    assert!(state.activity.tool_invocations[0].suppress_normal_card);
}

#[test]
fn tool_result_card_hides_opaque_output_ref_but_keeps_capture() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-tool-card-output-ref-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::with_raw_session_dir(&dir)
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-read".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                stream: "stdout".to_string(),
                text: "secret-ish\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let output_ref = dir.join("agent-output-refs/out-1.txt");
    assert!(output_ref.exists(), "output ref should still be captured");
    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!output.contains("output ref: out-1"), "{output}");
    assert!(!output.contains(output_ref.to_str().unwrap()), "{output}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tool_result_accumulator_tracks_output_metrics_and_first_error() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-tool-result-accumulator-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::with_raw_session_dir(&dir)
    };
    let large_stdout = format!("{}\n", "x".repeat(4_001));
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-read".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                stream: "stdout".to_string(),
                text: "line 1\nline 2\n".to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                stream: "stderr".to_string(),
                text: "fatal one\nfatal two\n".to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                stream: "stdout".to_string(),
                text: large_stdout,
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                status: "failed".to_string(),
            }),
        ],
    );

    let record = state
        .activity
        .tool_invocations
        .iter()
        .find(|record| record.invocation_id == "toolu-read")
        .expect("tool invocation");
    assert_eq!(record.output.stdout_lines, 3);
    assert_eq!(record.output.stderr_lines, 2);
    assert_eq!(record.output.stdout_bytes, 14 + 4_002);
    assert_eq!(record.output.stderr_bytes, 20);
    assert_eq!(record.output.first_error_line.as_deref(), Some("fatal one"));
    assert!(record.output.truncated);
    assert!(matches!(
        record.output.output_ref,
        Some(ToolOutputRef::DebugLocalPath { ref audit_ref, .. }) if audit_ref == "out-3"
    ));

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("fatal one"), "{output}");
    assert!(output.contains("stdout: 3 lines"), "{output}");
    assert!(output.contains("stderr: 2 lines"), "{output}");
    assert!(output.contains("truncated"), "{output}");
    assert!(!output.contains("output ref: out-3"), "{output}");
    assert!(!output.contains(&"x".repeat(200)), "{output}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn typed_tool_details_place_result_before_raw_input() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-read".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                stream: "stdout".to_string(),
                text: "line 1\nline 2\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-read".to_string(),
                status: "success".to_string(),
            }),
        ],
    );
    assert!(ids.iter().any(|id| id == "tool-2"), "{ids:?}");

    let mut output = Vec::new();
    render_activity_details_by_id(&state, "tool-2", &mut output)
        .expect("details result")
        .expect("render details");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("Tool:"), "{output}");
    assert!(output.contains("Canonical: Read"), "{output}");
    assert!(output.contains("Original: Read"), "{output}");
    assert!(output.contains("Impact: read-only"), "{output}");
    assert!(output.contains("Target:"), "{output}");
    assert!(output.contains("Primary: Cargo.toml"), "{output}");
    assert!(output.contains("Result:"), "{output}");
    assert!(output.contains("Headline: 2 lines returned"), "{output}");
    assert!(output.contains("Metric: stdout: 2 lines"), "{output}");
    assert!(output.contains("Raw input:"), "{output}");
    let result_pos = output.find("Result:").expect("result section");
    let raw_pos = output.find("Raw input:").expect("raw input section");
    assert!(result_pos < raw_pos, "{output}");
}

#[test]
fn typed_tool_details_caps_multi_file_paths_at_twenty() {
    let paths = (0..25)
        .map(|idx| format!(r#""file-{idx:02}.rs""#))
        .collect::<Vec<_>>()
        .join(",");
    let input = format!(r#"{{"paths":[{paths}]}}"#);
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-many".to_string()),
                name: "read_many_files".to_string(),
                input,
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-many".to_string(),
                status: "success".to_string(),
            }),
        ],
    );
    let detail_id = ids.last().expect("completion row");

    let mut output = Vec::new();
    render_activity_details_by_id(&state, detail_id, &mut output)
        .expect("details result")
        .expect("render details");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("Primary: 25 files"), "{output}");
    assert!(output.contains("path: file-00.rs"), "{output}");
    assert!(output.contains("path: file-19.rs"), "{output}");
    assert!(output.contains("omitted_paths: 5"), "{output}");
    assert!(!output.contains("file-20.rs"), "{output}");
}

#[test]
fn typed_tool_details_bound_custom_raw_input_in_normal_and_debug_modes() {
    let hidden_tail = "SECRET_RAW_INPUT_TAIL_SHOULD_NOT_RENDER";
    let long_body = format!("{}{}", "x".repeat(500), hidden_tail);
    let input = format!(r#"{{"title":"Bounded","body":"{long_body}"}}"#);
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-custom-raw".to_string()),
                name: "CustomTool".to_string(),
                input,
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-custom-raw".to_string(),
                status: "success".to_string(),
            }),
        ],
    );
    let detail_id = ids.last().expect("completion row").clone();

    let mut normal = Vec::new();
    render_activity_details_by_id(&state, &detail_id, &mut normal)
        .expect("details result")
        .expect("render details");
    let normal = String::from_utf8(normal).expect("utf8 output");
    assert!(normal.contains("Raw input:"), "{normal}");
    assert!(normal.contains("..."), "{normal}");
    assert!(!normal.contains(hidden_tail), "{normal}");

    state.debug = true;
    let mut debug = Vec::new();
    render_activity_details_by_id(&state, &detail_id, &mut debug)
        .expect("details result")
        .expect("render details");
    let debug = String::from_utf8(debug).expect("utf8 output");
    assert!(debug.contains("Audit row:"), "{debug}");
    assert!(!debug.contains(hidden_tail), "{debug}");
}

#[test]
fn shell_provider_tool_call_still_uses_shell_visibility_path() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: None,
            name: "run_shell_command".to_string(),
            input: "df -h".to_string(),
        })],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!output.contains("Activity"), "{output}");
    assert!(!output.contains("run_shell_command called"), "{output}");
}

#[test]
fn provider_native_shell_output_renders_transcript_without_activity_card() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-1".to_string()),
                name: "run_shell_command".to_string(),
                input: serde_json::json!({ "command": "df -h" }).to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-1".to_string(),
                stream: "stdout".to_string(),
                text: "Filesystem\n/dev/disk1\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-1".to_string(),
                status: "completed".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_provider_native_shell_transcript(&mut state, &ids, &mut output)
        .expect("render shell transcript");
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("$ df -h"), "{output}");
    assert!(output.contains("Filesystem\n/dev/disk1\n"), "{output}");
    assert!(!output.contains("Activity"), "{output}");
    assert!(
        !output.contains("stdout captured; [Details] out-1"),
        "{output}"
    );
    assert!(!output.contains("Tool completed"), "{output}");
    let detail = &state
        .activity
        .rows
        .iter()
        .find(|row| row.id == "out-1")
        .expect("output row")
        .detail;
    assert!(
        detail.contains("provider_native_shell_command: df -h"),
        "{detail}"
    );
}

#[test]
fn provider_native_shell_transcript_uses_structured_tool_state() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-1".to_string()),
                name: "run_shell_command".to_string(),
                input: serde_json::json!({ "command": "df -h" }).to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-1".to_string(),
                stream: "stdout".to_string(),
                text: "Filesystem\n/dev/disk1\n".to_string(),
            }),
        ],
    );
    let row = state
        .activity
        .rows
        .iter_mut()
        .find(|row| row.id == "out-1")
        .expect("output row");
    row.detail =
        "tool: toolu-1\nstream: stdout\noutput_ref: <hidden>\nDETAIL_ONLY_SHOULD_NOT_RENDER\n"
            .to_string();

    let mut output = Vec::new();
    render_provider_native_shell_transcript(&mut state, &ids, &mut output)
        .expect("render shell transcript");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("$ df -h"), "{output}");
    assert!(output.contains("Filesystem\n/dev/disk1\n"), "{output}");
    assert!(
        !output.contains("DETAIL_ONLY_SHOULD_NOT_RENDER"),
        "{output}"
    );
}

#[test]
fn provider_native_streamed_shell_output_renders_transcript_without_control_permission() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-shell".to_string()),
                name: "run_shell_command".to_string(),
                input: "df -h".to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-shell".to_string(),
                stream: "stdout".to_string(),
                text: "Filesystem\n/dev/disk1\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-shell".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_provider_native_shell_transcript(&mut state, &ids, &mut output)
        .expect("render shell transcript");
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("$ df -h"), "{output}");
    assert!(output.contains("Filesystem\n/dev/disk1\n"), "{output}");
    assert!(!output.contains("Activity"), "{output}");
    assert!(
        !output.contains("stdout captured; [Details] out-1"),
        "{output}"
    );
    let detail = &state
        .activity
        .rows
        .iter()
        .find(|row| row.id == "out-1")
        .expect("output row")
        .detail;
    assert!(
        detail.contains("provider_native_shell_command: df -h"),
        "{detail}"
    );
}

#[test]
fn control_protocol_policy_suppresses_provider_auto_approved_shell_activity() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows_with_policy(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-shell".to_string()),
                name: "run_shell_command".to_string(),
                input: "df -h".to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-shell".to_string(),
                stream: "stdout".to_string(),
                text: "Filesystem\n/dev/disk1\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-shell".to_string(),
                status: "success".to_string(),
            }),
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-read".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            }),
        ],
        ActivityRecordPolicy {
            suppress_provider_native_shell: true,
            ..Default::default()
        },
    );

    let mut output = Vec::new();
    render_provider_native_shell_transcript(&mut state, &ids, &mut output)
        .expect("render shell transcript");
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("$ df -h"), "{output}");
    assert!(output.contains("Filesystem\n/dev/disk1\n"), "{output}");
    assert!(
        !output.contains("run_shell_command auto-approved by provider"),
        "{output}"
    );
    assert!(!output.contains("Bash completed"), "{output}");
    assert!(!output.contains("Read called: Cargo.toml"), "{output}");
    assert!(state.activity.rows.iter().any(|row| {
        row.detail.contains("evidence: ProviderNativeShellBypass")
            && row
                .detail
                .contains("provider_native_shell_bypassed_control_protocol")
            && row
                .detail
                .contains("provider_auto_approval_status: auto_approved_by_provider")
            && row.detail.contains("provider_native_shell_command: df -h")
    }));
}

#[test]
fn debug_mode_keeps_provider_auto_approved_shell_activity() {
    let mut state = InlineState {
        language: Language::EnUs,
        debug: true,
        ..InlineState::default()
    };
    let ids = record_activity_rows_with_policy(
        &mut state,
        &[governed(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-shell".to_string()),
            name: "run_shell_command".to_string(),
            input: "df -h".to_string(),
        })],
        ActivityRecordPolicy {
            suppress_provider_native_shell: true,
            ..Default::default()
        },
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("Shell auto-approved: $ df -h"), "{output}");
    assert!(!output.contains("[Details]"), "{output}");
}

#[test]
fn question_tool_call_is_hidden_when_question_card_handles_it() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-question".to_string()),
            name: "ask_user_question".to_string(),
            input: serde_json::json!({
                "question": "Pick one",
                "options": [{"label": "A"}, {"label": "B"}]
            })
            .to_string(),
        })],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!output.contains("Activity"), "{output}");
    assert!(!output.contains("ask_user_question called"), "{output}");
}

#[test]
fn control_permission_tool_request_is_hidden_when_approval_card_handles_it() {
    let mut state = InlineState {
        language: Language::EnUs,
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolPermissionRequest {
            run_id: "run-1".to_string(),
            request_id: "ctrl-write".to_string(),
            tool_name: "Write".to_string(),
            tool_input: serde_json::json!({
                "file_path": "/tmp/cosh-write.txt",
                "content": "ok"
            }),
            tool_use_id: "toolu-write".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        })],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!output.contains("Activity"), "{output}");
    assert!(!output.contains("Write requested"), "{output}");
}

#[test]
fn matching_tool_call_is_hidden_when_control_permission_card_handles_it() {
    let mut state = InlineState {
        language: Language::EnUs,
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-write".to_string()),
                name: "Write".to_string(),
                input: serde_json::json!({
                    "file_path": "/tmp/cosh-write.txt",
                    "content": "ok"
                })
                .to_string(),
            }),
            governed(AgentEvent::ToolPermissionRequest {
                run_id: "run-1".to_string(),
                request_id: "ctrl-write".to_string(),
                tool_name: "Write".to_string(),
                tool_input: serde_json::json!({
                    "file_path": "/tmp/cosh-write.txt",
                    "content": "ok"
                }),
                tool_use_id: "toolu-write".to_string(),
                hook_requires_approval: false,
                audit_ref: None,
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!output.contains("Activity"), "{output}");
    assert!(!output.contains("Write called"), "{output}");
    assert!(!output.contains("Write requested"), "{output}");
}

#[test]
fn recommend_mode_keeps_visible_control_permission_row_for_matching_tool_call() {
    let mut state = InlineState {
        language: Language::EnUs,
        approval_mode: CoshApprovalMode::Recommend,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-read".to_string()),
                name: "Read".to_string(),
                input: serde_json::json!({ "file_path": "Cargo.toml" }).to_string(),
            }),
            governed(AgentEvent::ToolPermissionRequest {
                run_id: "run-1".to_string(),
                request_id: "ctrl-read".to_string(),
                tool_name: "Read".to_string(),
                tool_input: serde_json::json!({ "file_path": "Cargo.toml" }),
                tool_use_id: "toolu-read".to_string(),
                hook_requires_approval: false,
                audit_ref: None,
            }),
        ],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!output.contains("Read called"), "{output}");
    assert!(output.contains("Read requested: Cargo.toml"), "{output}");
}

#[test]
fn recommend_mode_keeps_control_permission_tool_request_activity_visible() {
    let mut state = InlineState {
        language: Language::EnUs,
        approval_mode: CoshApprovalMode::Recommend,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ToolPermissionRequest {
            run_id: "run-1".to_string(),
            request_id: "ctrl-write".to_string(),
            tool_name: "Write".to_string(),
            tool_input: serde_json::json!({
                "file_path": "/tmp/cosh-write.txt",
                "content": "ok"
            }),
            tool_use_id: "toolu-write".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        })],
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(
        output.contains("Write requested: /tmp/cosh-write.txt"),
        "{output}"
    );
}

#[test]
fn control_protocol_policy_suppresses_known_foreground_shell_echo() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    state
        .control
        .mark_provider_shell_transcript_seen("run-1", "toolu-shell");
    let ids = record_activity_rows_with_policy(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-shell".to_string()),
                name: "run_shell_command".to_string(),
                input: r#"{"command":"df -h"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-shell".to_string(),
                stream: "stdout".to_string(),
                text: "Filesystem\n/dev/disk1\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-shell".to_string(),
                status: "success".to_string(),
            }),
        ],
        ActivityRecordPolicy {
            suppress_provider_native_shell: true,
            ..Default::default()
        },
    );

    assert!(ids.is_empty(), "{ids:?}");
    assert!(state.activity.rows.is_empty(), "{:?}", state.activity.rows);
}

#[test]
fn control_permission_shell_output_is_not_rendered_as_provider_native_transcript() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolPermissionRequest {
                run_id: "run-1".to_string(),
                request_id: "ctrl-1".to_string(),
                tool_name: "run_shell_command".to_string(),
                tool_input: serde_json::json!({ "command": "ssh -V" }),
                tool_use_id: "toolu-shell".to_string(),
                hook_requires_approval: false,
                audit_ref: None,
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "toolu-shell".to_string(),
                stream: "stdout".to_string(),
                text: "PROVIDER OUTPUT SHOULD NOT RENDER\n".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_provider_native_shell_transcript(&mut state, &ids, &mut output)
        .expect("render shell transcript");
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!output.contains("$ ssh -V"), "{output}");
    assert!(
        !output.contains("PROVIDER OUTPUT SHOULD NOT RENDER"),
        "{output}"
    );
}

#[test]
fn provider_native_streamed_shell_output_uses_tool_id_not_pending_order() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("tool-first".to_string()),
                name: "run_shell_command".to_string(),
                input: r#"{"command":"echo FIRST"}"#.to_string(),
            }),
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("tool-second".to_string()),
                name: "run_shell_command".to_string(),
                input: r#"{"command":"echo SECOND"}"#.to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "tool-second".to_string(),
                stream: "stdout".to_string(),
                text: "SECOND\n".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_provider_native_shell_transcript(&mut state, &ids, &mut output)
        .expect("render shell transcript");
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("$ echo SECOND\nSECOND\n"), "{output}");
    assert!(!output.contains("$ echo FIRST\nSECOND"), "{output}");
    assert!(!output.contains("Activity"), "{output}");
}

#[test]
fn provider_native_shell_success_without_output_uses_receipt_card() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-1".to_string()),
                name: "run_shell_command".to_string(),
                input: serde_json::json!({ "command": "true" }).to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-1".to_string(),
                status: "success".to_string(),
            }),
        ],
    );

    let cards = tool_invocation_cards_for_test(&state, &ids);
    assert_eq!(cards.len(), 1);
    assert_eq!(cards[0].density, ToolInvocationDensity::Receipt);

    let mut output = Vec::new();
    render_provider_native_shell_transcript(&mut state, &ids, &mut output)
        .expect("render shell transcript");
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("Shell completed"), "{output}");
    assert!(output.contains("$ true"), "{output}");
    assert!(!output.starts_with("$ true\n"), "{output}");
    assert!(!output.contains("tool status:"), "{output}");
}

#[test]
fn provider_native_shell_error_completion_uses_transcript_not_activity() {
    let mut state = InlineState {
        language: Language::EnUs,
        ..InlineState::default()
    };
    let ids = record_activity_rows(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-1".to_string()),
                name: "run_shell_command".to_string(),
                input: serde_json::json!({ "command": "df -h" }).to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "toolu-1".to_string(),
                status: "error".to_string(),
            }),
        ],
    );

    let mut output = Vec::new();
    render_provider_native_shell_transcript(&mut state, &ids, &mut output)
        .expect("render shell transcript");
    render_activity_rows(&state, &ids, &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(output.contains("$ df -h"), "{output}");
    assert!(output.contains("tool status: error"), "{output}");
    assert!(!output.contains("Activity"), "{output}");
    assert!(!output.contains("Tool error"), "{output}");
}

#[test]
fn tool_output_ref_uses_private_permissions() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-activity-output-ref-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);

    let path = write_tool_output_ref(&dir, "out-1", "secret-ish\n").expect("write output ref");

    assert_eq!(
        std::fs::metadata(&dir)
            .expect("dir metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(&path)
            .expect("file metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn shell_handoff_activity_marks_user_interrupt_status() {
    let mut state = InlineState::default();
    let request = ShellHandoffRequest::new(
        "sleep 100",
        "$ sleep 100",
        "approved_provider_shell_tool",
        "user",
        "req-1",
        "run-1",
        0,
    )
    .expect("handoff request");
    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(request);
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(0)
        .expect("emit pending handoff");
    let block = CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "session-1".to_string(),
        command: "sleep 100".to_string(),
        origin: CommandOrigin::ProviderTool,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 1,
        ended_at_ms: 10,
        duration_ms: 9,
        exit_code: 130,
        status: CommandStatus::Failed,
        output: OutputRefs {
            terminal_output_ref: Some("/tmp/internal-output-ref.txt".to_string()),
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };

    let ids = record_approved_shell_handoff_blocks(&mut state, &[block]);

    assert_eq!(ids, vec!["handoff-1"]);
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == "handoff-1")
        .expect("handoff row");
    assert_eq!(row.status, "interrupted");
    assert!(row.detail.contains("status: interrupted"), "{}", row.detail);
    assert!(row.detail.contains("exit_code: 130"), "{}", row.detail);
    assert!(
        row.detail
            .contains("output_id: terminal-output://session-1/cmd-1"),
        "{}",
        row.detail
    );
}

#[test]
fn shell_handoff_activity_ignores_stale_same_command_block_before_request() {
    let mut state = InlineState::default();
    let request = ShellHandoffRequest::new(
        "df -h",
        "$ df -h",
        "approved_provider_shell_tool",
        "user",
        "req-stale",
        "run-stale",
        1_000,
    )
    .expect("handoff request");
    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(request);
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(0)
        .expect("emit pending handoff");
    let stale_block = CommandBlock {
        id: "cmd-stale".to_string(),
        session_id: "session-1".to_string(),
        command: "df -h".to_string(),
        origin: CommandOrigin::ProviderTool,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 100,
        ended_at_ms: 200,
        duration_ms: 100,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: Some("/tmp/stale-output-ref.txt".to_string()),
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };

    let ids = record_approved_shell_handoff_blocks(&mut state, &[stale_block]);

    assert!(ids.is_empty(), "{ids:?}");
    assert!(state.activity.rows.is_empty(), "{:?}", state.activity.rows);
    assert!(state.control.shell_handoff().pending_front().is_some());
}

#[test]
fn repeated_shell_handoff_uses_the_unhandled_current_block() {
    let mut state = InlineState::default();
    let first_request = ShellHandoffRequest::new(
        "./changing_metric.sh",
        "$ ./changing_metric.sh",
        "approved_provider_shell_tool",
        "user",
        "req-1",
        "run-1",
        1_100,
    )
    .expect("first handoff request");
    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(first_request);
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(0)
        .expect("emit first handoff");
    let first_block = CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "session-1".to_string(),
        command: "./changing_metric.sh".to_string(),
        origin: CommandOrigin::ProviderTool,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 1_000,
        ended_at_ms: 1_000,
        duration_ms: 0,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: Some("/tmp/output-1001.txt".to_string()),
            terminal_output_bytes: 20,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    assert_eq!(
        record_approved_shell_handoff_blocks(&mut state, std::slice::from_ref(&first_block)),
        vec!["handoff-1"]
    );

    let second_request = ShellHandoffRequest::new(
        "./changing_metric.sh",
        "$ ./changing_metric.sh",
        "approved_provider_shell_tool",
        "user",
        "req-2",
        "run-2",
        1_500,
    )
    .expect("second handoff request");
    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(second_request);
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(0)
        .expect("emit second handoff");
    let second_block = CommandBlock {
        id: "cmd-2".to_string(),
        session_id: "session-1".to_string(),
        command: "./changing_metric.sh".to_string(),
        origin: CommandOrigin::ProviderTool,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 1_000,
        ended_at_ms: 1_000,
        duration_ms: 0,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: Some("/tmp/output-1002.txt".to_string()),
            terminal_output_bytes: 20,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };

    assert_eq!(
        record_approved_shell_handoff_blocks(&mut state, &[first_block, second_block]),
        vec!["handoff-2"]
    );
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == "handoff-2")
        .expect("second handoff row");
    assert!(
        row.detail.contains("command_block: cmd-2"),
        "{}",
        row.detail
    );
    assert!(
        row.detail
            .contains("output_id: terminal-output://session-1/cmd-2"),
        "{}",
        row.detail
    );
}

fn untracked_test_event(kind: ShellEventKind, command_id: Option<&str>) -> ShellEvent {
    ShellEvent {
        kind,
        session_id: "session-1".to_string(),
        command_id: command_id.map(str::to_string),
        command: None,
        cwd: Some("/tmp".to_string()),
        end_cwd: None,
        exit_code: None,
        started_at_ms: Some(1),
        ended_at_ms: None,
        duration_ms: None,
        terminal_output_ref: None,
        terminal_output_bytes: None,
        input: None,
        component: None,
        message: None,
        command_origin: None,
        shell_environment_generation: None,
        audit_identity: None,
        routing: None,
        capture: None,
    }
}

fn untracked_test_handoff_state(emitted_at_event_index: usize) -> InlineState {
    let mut state = InlineState::default();
    let request = ShellHandoffRequest::new(
        "ls -la /root/",
        "$ ls -la /root/",
        "approved_provider_shell_tool",
        "user",
        "req-untracked",
        "run-untracked",
        0,
    )
    .expect("handoff request");
    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(request);
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(emitted_at_event_index)
        .expect("emit pending handoff");
    state
}

#[test]
fn untracked_shell_handoff_closes_on_shell_ready_after_emit() {
    let mut state = untracked_test_handoff_state(2);
    let events = vec![
        untracked_test_event(ShellEventKind::ShellReady, None),
        untracked_test_event(ShellEventKind::ShellReady, None),
        untracked_test_event(ShellEventKind::ShellReady, None),
    ];

    let ids = close_untracked_shell_handoffs(&mut state, &events);

    assert_eq!(ids, vec!["handoff-1"]);
    assert!(state.control.shell_handoff().pending_front().is_none());
    let evidence = state
        .evidence
        .latest_shell_command_completed()
        .expect("degraded evidence");
    assert_eq!(evidence.status, "completed_untracked");
    // Decision: the degraded path never reports a real exit code or output.
    assert_eq!(evidence.exit_code, -1);
    assert!(evidence.terminal_output_ref.is_none());
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == "handoff-1")
        .expect("handoff row");
    assert_eq!(row.status, "completed_untracked");
    assert!(
        row.detail.contains("preexec_marker_missing"),
        "{}",
        row.detail
    );
    assert!(
        row.detail.contains("exit_code: unavailable"),
        "{}",
        row.detail
    );
    assert!(row.detail.contains("output_id: <none>"), "{}", row.detail);
    // The degraded evidence must feed the existing continuation recovery.
    assert_eq!(
        state
            .evidence
            .claim_pending_shell_handoff_continuations()
            .len(),
        1
    );
}

#[test]
fn untracked_shell_handoff_ignores_shell_ready_before_emit() {
    let mut state = untracked_test_handoff_state(2);
    let events = vec![
        untracked_test_event(ShellEventKind::ShellReady, None),
        untracked_test_event(ShellEventKind::ShellReady, None),
    ];

    let ids = close_untracked_shell_handoffs(&mut state, &events);

    assert!(ids.is_empty(), "{ids:?}");
    assert!(state.control.shell_handoff().pending_front().is_some());
    assert!(state.evidence.latest_shell_command_completed().is_none());
}

#[test]
fn untracked_shell_handoff_defers_to_tracked_command_path() {
    let mut state = untracked_test_handoff_state(0);
    let events = vec![
        untracked_test_event(ShellEventKind::CommandStarted, Some("cmd-1")),
        untracked_test_event(ShellEventKind::ShellReady, None),
    ];

    let ids = close_untracked_shell_handoffs(&mut state, &events);

    assert!(ids.is_empty(), "{ids:?}");
    assert!(state.control.shell_handoff().pending_front().is_some());
}

#[test]
fn untracked_shell_handoff_survives_command_after_prompt_boundary() {
    // Regression: the event list is cumulative, so a command that starts
    // after the handoff's prompt boundary was already reached must not
    // veto that boundary; the first relevant boundary decides ownership.
    let mut state = untracked_test_handoff_state(0);
    let events = vec![
        untracked_test_event(ShellEventKind::ShellReady, None),
        untracked_test_event(ShellEventKind::CommandStarted, Some("cmd-later")),
    ];

    let ids = close_untracked_shell_handoffs(&mut state, &events);

    assert_eq!(ids, vec!["handoff-1"]);
    assert!(state.control.shell_handoff().pending_front().is_none());
    let evidence = state
        .evidence
        .latest_shell_command_completed()
        .expect("degraded evidence");
    assert_eq!(evidence.status, "completed_untracked");
}

#[test]
fn untracked_shell_handoff_closure_is_idempotent_on_cumulative_events() {
    // Regression: replaying the same cumulative event list after closure
    // must not produce duplicate rows, evidence, or continuations.
    let mut state = untracked_test_handoff_state(0);
    let events = vec![
        untracked_test_event(ShellEventKind::ShellReady, None),
        untracked_test_event(ShellEventKind::CommandStarted, Some("cmd-later")),
    ];

    let first = close_untracked_shell_handoffs(&mut state, &events);
    assert_eq!(first, vec!["handoff-1"]);
    let continuations = state.evidence.claim_pending_shell_handoff_continuations();
    assert_eq!(continuations.len(), 1);

    let second = close_untracked_shell_handoffs(&mut state, &events);

    assert!(second.is_empty(), "{second:?}");
    assert_eq!(
        state
            .activity
            .rows
            .iter()
            .filter(|row| row.id == "handoff-1")
            .count(),
        1
    );
    assert!(state
        .evidence
        .claim_pending_shell_handoff_continuations()
        .is_empty());
}

#[test]
fn untracked_shell_handoff_noop_without_prompt_boundary() {
    let mut state = untracked_test_handoff_state(0);
    let events: Vec<ShellEvent> = Vec::new();

    let ids = close_untracked_shell_handoffs(&mut state, &events);

    assert!(ids.is_empty(), "{ids:?}");
    assert!(state.control.shell_handoff().pending_front().is_some());
}

#[test]
fn untracked_shell_handoff_closure_leaves_interactive_handoffs_untouched() {
    // Boundary: the untracked fallback only operates on the emitted
    // shell-handoff queue; interactive shell handoffs (user-executed
    // suggestions) live in a separate store and may legitimately stay
    // pending across many prompt boundaries.
    let mut state = untracked_test_handoff_state(0);
    record_activity_rows_with_policy(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-untracked".to_string(),
                tool_id: Some("tool-use-1".to_string()),
                name: "Bash".to_string(),
                input: serde_json::json!({ "command": "sudo systemctl status sshd" }).to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-untracked".to_string(),
                tool_id: "tool-use-1".to_string(),
                stream: "stderr".to_string(),
                text: "sudo: a terminal is required\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-untracked".to_string(),
                tool_id: "tool-use-1".to_string(),
                status: "error".to_string(),
            }),
        ],
        ActivityRecordPolicy::default(),
    );
    let interactive_before = state
        .control
        .interactive_shell_handoff_ids()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert!(!interactive_before.is_empty());
    let events = vec![untracked_test_event(ShellEventKind::ShellReady, None)];

    let ids = close_untracked_shell_handoffs(&mut state, &events);

    assert_eq!(ids.len(), 1, "{ids:?}");
    assert!(state.control.shell_handoff().pending_front().is_none());
    let interactive_after = state
        .control
        .interactive_shell_handoff_ids()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(interactive_after, interactive_before);
}

#[test]
fn untracked_shell_handoffs_close_on_distinct_prompt_boundaries() {
    // The transport owns one request sidecar and claim slot, so the second
    // request remains approved until the first closes at its prompt boundary.
    let mut state = InlineState::default();
    for (command, approval_id) in [("ls -la /root/", "req-a"), ("df -h", "req-b")] {
        let request = ShellHandoffRequest::new(
            command,
            format!("$ {command}"),
            "approved_provider_shell_tool",
            "user",
            approval_id,
            "run-untracked",
            0,
        )
        .expect("handoff request");
        state
            .control
            .shell_handoff_mut()
            .enqueue_approved_request(request);
    }
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(2)
        .expect("emit first handoff");
    assert!(state
        .control
        .shell_handoff_mut()
        .emit_next_approved(2)
        .is_none());
    let mut events = vec![
        untracked_test_event(ShellEventKind::ShellReady, None),
        untracked_test_event(ShellEventKind::ShellReady, None),
        untracked_test_event(ShellEventKind::ShellReady, None),
    ];

    let first_ids = close_untracked_shell_handoffs(&mut state, &events);
    assert_eq!(first_ids, vec!["handoff-1"]);
    assert!(state.control.shell_handoff().pending_front().is_none());

    let second = state
        .control
        .shell_handoff_mut()
        .emit_next_approved(events.len())
        .expect("emit second handoff after first closure");
    assert_eq!(second.approval_id, "req-b");
    assert!(close_untracked_shell_handoffs(&mut state, &events).is_empty());

    events.push(untracked_test_event(ShellEventKind::ShellReady, None));
    let second_ids = close_untracked_shell_handoffs(&mut state, &events);

    assert_eq!(second_ids, vec!["handoff-2"]);
    assert!(state.control.shell_handoff().pending_front().is_none());
    for id in ["handoff-1", "handoff-2"] {
        let row = state
            .activity
            .rows
            .iter()
            .find(|row| row.id == id)
            .expect("handoff row");
        assert_eq!(row.status, "completed_untracked");
    }
    // Both closures produced recoverable evidence, and neither is lost.
    assert_eq!(
        state
            .evidence
            .claim_pending_shell_handoff_continuations()
            .len(),
        1
    );
    assert_eq!(
        state
            .evidence
            .claim_pending_shell_handoff_continuations()
            .len(),
        1
    );
    assert!(state
        .evidence
        .claim_pending_shell_handoff_continuations()
        .is_empty());
}

#[test]
fn untracked_shell_handoff_emitted_after_boundary_stays_pending() {
    // A handoff emitted strictly after the latest prompt boundary must not
    // be closed by an earlier ShellReady: that boundary belongs to the
    // previous prompt cycle and says nothing about the new handoff.
    let mut state = InlineState::default();
    for (command, approval_id) in [("ls -la /root/", "req-a"), ("df -h", "req-b")] {
        let request = ShellHandoffRequest::new(
            command,
            format!("$ {command}"),
            "approved_provider_shell_tool",
            "user",
            approval_id,
            "run-untracked",
            0,
        )
        .expect("handoff request");
        state
            .control
            .shell_handoff_mut()
            .enqueue_approved_request(request);
    }
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(1)
        .expect("emit first handoff");
    let events = vec![
        untracked_test_event(ShellEventKind::ShellReady, None),
        untracked_test_event(ShellEventKind::ShellReady, None),
        untracked_test_event(ShellEventKind::ShellReady, None),
    ];
    assert_eq!(
        close_untracked_shell_handoffs(&mut state, &events),
        vec!["handoff-1"]
    );
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(3)
        .expect("emit second handoff");

    let ids = close_untracked_shell_handoffs(&mut state, &events);

    assert!(ids.is_empty(), "{ids:?}");
    let pending = state
        .control
        .shell_handoff()
        .pending_front()
        .expect("second handoff stays pending");
    assert_eq!(pending.request().approval_id, "req-b");
}

#[test]
fn activity_interactive_handoff_summary_uses_state_language() {
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };
    record_activity_rows_with_policy(
        &mut state,
        &[
            governed(AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("tool-use-1".to_string()),
                name: "Bash".to_string(),
                input: serde_json::json!({ "command": "sudo systemctl status sshd" }).to_string(),
            }),
            governed(AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "tool-use-1".to_string(),
                stream: "stderr".to_string(),
                text: "sudo: a terminal is required\n".to_string(),
            }),
            governed(AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "tool-use-1".to_string(),
                status: "error".to_string(),
            }),
        ],
        ActivityRecordPolicy {
            origin: AgentRunOrigin::InsightPrompt,
            ..ActivityRecordPolicy::default()
        },
    );

    let handoff = state
        .control
        .find_interactive_shell_handoff("handoff-1")
        .expect("interactive handoff");
    assert_eq!(handoff.origin, AgentRunOrigin::InsightPrompt);

    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == "tool-2")
        .expect("activity row");
    assert_eq!(
        row.summary,
        "sudo: a terminal is required; 可能需要前台 shell；[Send to shell] handoff-1"
    );
    assert!(!row.summary.contains("tool-2"));
    assert!(row
        .detail
        .contains("interactive_hint: may_require_foreground_shell"));
}

#[test]
fn activity_records_shell_evidence_read_details() {
    let mut state = InlineState::default();

    let id = record_shell_evidence_action(
        state.language,
        &mut state.activity.rows,
        &mut state.activity.tool_invocations,
        "run-1",
        "read-1",
        "toolu-1",
        "read_output",
        Some("terminal-output://raw-session-123/cmd-1"),
        Some("tail"),
        Some(120),
        "unavailable",
        Some("stale_session"),
        Some("ps aux | head -n 50"),
        None,
        false,
        false,
    );

    assert_eq!(id, "evidence-1");
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == id)
        .expect("activity row");
    assert_eq!(row.subject, "toolu-1");
    assert!(row.summary.contains("cosh_shell_evidence"), "{row:?}");
    assert!(row.detail.contains("evidence: ShellEvidenceAction"));
    assert!(row.detail.contains("request_id: read-1"));
    assert!(row.detail.contains("tool_use_id: toolu-1"));
    assert!(row.detail.contains("action: read_output"));
    assert!(row
        .detail
        .contains("output_id: terminal-output://raw-session-123/cmd-1"));
    assert!(row.detail.contains("status: unavailable"));
    assert!(row.detail.contains("failure_reason: stale_session"));
    assert!(row.detail.contains("command: ps aux | head -n 50"));
    assert!(!row.summary.contains("read_output"), "{row:?}");
    assert!(!row.summary.contains("terminal-output://"), "{row:?}");

    let mut output = Vec::new();
    render_activity_rows(&state, &[id], &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("Shell evidence failed"), "{output}");
    assert!(output.contains("shell evidence unavailable"), "{output}");
    assert!(output.contains("ps aux | head -n 50"), "{output}");
    assert!(output.contains("tail 120 lines"), "{output}");
    assert!(output.contains("reason: stale_session"), "{output}");
    assert!(!output.contains("read_output"), "{output}");
    assert!(!output.contains("terminal-output://"), "{output}");
    assert!(!output.contains("output ref:"), "{output}");
}

#[test]
fn activity_reuses_shell_evidence_row_by_structured_tool_id() {
    let mut state = InlineState::default();

    let id = record_shell_evidence_action(
        state.language,
        &mut state.activity.rows,
        &mut state.activity.tool_invocations,
        "run-1",
        "read-1",
        "toolu-1",
        "read_output",
        Some("terminal-output://raw-session-123/cmd-1"),
        Some("tail"),
        Some(120),
        "available",
        None,
        Some("ps aux | head -n 50"),
        None,
        false,
        false,
    );
    state
        .activity
        .rows
        .iter_mut()
        .find(|row| row.id == id)
        .expect("shell evidence row")
        .detail = "legacy detail without request fields".to_string();

    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ShellEvidenceRequest {
            run_id: "run-1".to_string(),
            request_id: "read-1".to_string(),
            tool_use_id: "toolu-1".to_string(),
            action: crate::adapter::ShellEvidenceAction::ReadOutput {
                output_id: "terminal-output://raw-session-123/cmd-1".to_string(),
                direction: crate::adapter::ShellOutputDirection::Tail,
                lines: 120,
                bypass_recent_filter: false,
            },
        })],
    );

    assert_eq!(ids, vec![id]);
}

#[test]
fn activity_shell_evidence_row_falls_back_to_request_id_without_tool_id() {
    let mut state = InlineState::default();

    let id = record_shell_evidence_action(
        state.language,
        &mut state.activity.rows,
        &mut state.activity.tool_invocations,
        "run-1",
        "read-1",
        "",
        "list_commands",
        None,
        None,
        None,
        "included",
        None,
        None,
        Some(2),
        false,
        false,
    );

    assert_eq!(state.activity.tool_invocations[0].invocation_id, "read-1");
    assert_eq!(state.activity.rows[0].subject, "read-1");

    let ids = record_activity_rows(
        &mut state,
        &[governed(AgentEvent::ShellEvidenceRequest {
            run_id: "run-1".to_string(),
            request_id: "read-1".to_string(),
            tool_use_id: String::new(),
            action: crate::adapter::ShellEvidenceAction::ListCommands {
                limit: 20,
                cursor: None,
            },
        })],
    );

    assert_eq!(ids, vec![id]);
}

#[test]
fn activity_shell_evidence_fallback_row_hides_protocol_fields() {
    let mut state = InlineState::default();

    let id = record_shell_evidence_action(
        state.language,
        &mut state.activity.rows,
        &mut state.activity.tool_invocations,
        "run-1",
        "read-1",
        "toolu-1",
        "read_output",
        Some("terminal-output://raw-session-123/cmd-1"),
        Some("tail"),
        Some(120),
        "failed",
        Some("stale_session"),
        Some("ps aux | head -n 50"),
        None,
        false,
        false,
    );
    state.activity.tool_invocations.clear();

    let mut output = Vec::new();
    render_activity_rows(&state, &[id], &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("Shell evidence failed"), "{output}");
    assert!(output.contains("ps aux | head -n 50"), "{output}");
    assert!(output.contains("tail 120 lines"), "{output}");
    assert!(output.contains("reason: stale_session"), "{output}");
    assert!(!output.contains("read_output"), "{output}");
    assert!(!output.contains("terminal-output://"), "{output}");
}

#[test]
fn activity_shell_evidence_success_card_hides_protocol_fields() {
    let mut state = InlineState::default();

    let id = record_shell_evidence_action(
        state.language,
        &mut state.activity.rows,
        &mut state.activity.tool_invocations,
        "run-1",
        "read-1",
        "toolu-1",
        "read_output",
        Some("terminal-output://raw-session-123/cmd-1"),
        Some("tail"),
        Some(120),
        "available",
        None,
        Some("top -l 1 -s 0 | head -12"),
        None,
        false,
        false,
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &[id], &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("Shell evidence completed"), "{output}");
    assert!(
        output.contains("shell output excerpt delivered to Agent"),
        "{output}"
    );
    assert!(
        output.contains("#cmd-1 $ top -l 1 -s 0 | head -12"),
        "{output}"
    );
    assert!(output.contains("top -l 1 -s 0 | head -12"), "{output}");
    assert!(output.contains("tail 120 lines"), "{output}");
    assert!(!output.contains("read_output"), "{output}");
    assert!(!output.contains("reason:"), "{output}");
    assert!(!output.contains("<none>"), "{output}");
    assert!(!output.contains("terminal-output://"), "{output}");
    assert!(!output.contains("output ref:"), "{output}");
}

#[test]
fn activity_shell_evidence_list_card_shows_command_count() {
    let mut state = InlineState {
        language: Language::ZhCn,
        ..InlineState::default()
    };

    let id = record_shell_evidence_action(
        state.language,
        &mut state.activity.rows,
        &mut state.activity.tool_invocations,
        "run-1",
        "list-1",
        "toolu-list-1",
        "list_commands",
        None,
        None,
        None,
        "included",
        None,
        None,
        Some(5),
        true,
        false,
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &[id], &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(output.contains("Shell 证据 已完成"), "{output}");
    assert!(output.contains("命令历史已交给 Agent"), "{output}");
    assert!(output.contains("命令 5 条"), "{output}");
    assert!(output.contains("还有更多历史"), "{output}");
    assert!(!output.contains("list_commands"), "{output}");
    assert!(!output.contains("原因:"), "{output}");
    assert!(!output.contains("<none>"), "{output}");
    assert!(!output.contains("cursor"), "{output}");
    assert!(!output.contains("terminal-output://"), "{output}");
}

#[test]
fn activity_shell_evidence_duplicate_request_is_visible_as_duplicate() {
    let mut state = InlineState::default();

    let id = record_shell_evidence_action(
        state.language,
        &mut state.activity.rows,
        &mut state.activity.tool_invocations,
        "run-1",
        "read-2",
        "toolu-2",
        "read_output",
        Some("terminal-output://raw-session-123/cmd-2"),
        Some("tail"),
        Some(120),
        "redacted_confirmation_required",
        Some("redacted_confirmation_required"),
        Some("ps aux | head -n 50"),
        None,
        false,
        true,
    );

    let mut output = Vec::new();
    render_activity_rows(&state, &[id], &mut output).expect("render activity");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(
        output.contains("Shell evidence duplicate request"),
        "{output}"
    );
    assert!(
        output.contains("provider repeated the same shell evidence request"),
        "{output}"
    );
    assert!(output.contains("ps aux | head -n 50"), "{output}");
    assert!(
        output.contains("reason: redacted_confirmation_required"),
        "{output}"
    );
}

#[test]
fn activity_records_terminal_output_read_misroute_for_control_tool_mode() {
    let mut state = InlineState::default();
    let ids = record_activity_rows_with_policy(
        &mut state,
        &[governed(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-read".to_string()),
            name: "read_file".to_string(),
            input: r#"{"path":"terminal-output://raw-session-123/cmd-1"}"#.to_string(),
        })],
        ActivityRecordPolicy {
            suppress_provider_native_shell: false,
            shell_evidence_tool_available: true,
            ..ActivityRecordPolicy::default()
        },
    );

    assert_eq!(ids, ["tool-1"]);
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == "tool-1")
        .expect("activity row");
    assert!(row.detail.contains("virtual_evidence_read_misroute: true"));
    assert!(row
        .detail
        .contains("misrouted_output_id: terminal-output://raw-session-123/cmd-1"));
    assert!(row
        .detail
        .contains("recommended_action: cosh_shell_evidence_read_output"));

    let mut output = Vec::new();
    render_activity_details_by_id(&state, "tool-1", &mut output)
        .expect("details result")
        .expect("render details");
    let output = String::from_utf8(output).expect("utf8 output");
    assert!(
        output.contains("Primary: Shell output bookmark"),
        "{output}"
    );
    assert!(
        output.contains("virtual_evidence_read_misroute: true"),
        "{output}"
    );
    assert!(
        output.contains("misrouted_output_id: terminal-output://raw-session-123/cmd-1"),
        "{output}"
    );
    assert!(
        output.contains("recommended_action: cosh_shell_evidence_read_output"),
        "{output}"
    );
}

#[test]
fn activity_records_terminal_output_read_misroute_for_fenced_fallback() {
    let mut state = InlineState::default();
    let ids = record_activity_rows_with_policy(
        &mut state,
        &[governed(AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-read".to_string()),
            name: "Read".to_string(),
            input: r#"{"file_path":"terminal-output://raw-session-123/cmd-1"}"#.to_string(),
        })],
        ActivityRecordPolicy {
            suppress_provider_native_shell: false,
            shell_evidence_tool_available: false,
            ..ActivityRecordPolicy::default()
        },
    );

    assert_eq!(ids, ["tool-1"]);
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == "tool-1")
        .expect("activity row");
    assert!(row.detail.contains("virtual_evidence_read_misroute: true"));
    assert!(row
        .detail
        .contains("recommended_action: fenced_cosh_request_output"));
}

// #2142 review R5: a block carrying an explicit handoff token must decide by
// that token alone. A block carrying another request's token must not be
// text-matched against the pending request, even when the commands are equal.
#[test]
fn block_with_another_requests_token_does_not_close_the_front_handoff() {
    let mut state = InlineState::default();
    let command = "echo twin-handoff";
    let mut requests = Vec::new();
    for (approval, run) in [("req-a", "run-a"), ("req-b", "run-b")] {
        let request = ShellHandoffRequest::new(
            command,
            format!("$ {command}"),
            "approved_provider_shell_tool",
            "user",
            approval,
            run,
            0,
        )
        .expect("handoff request");
        requests.push(request.clone());
        if approval == "req-a" {
            state
                .control
                .shell_handoff_mut()
                .enqueue_approved_request(request);
        }
    }
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(0)
        .expect("emit first handoff");
    let block = CommandBlock {
        id: "cmd-second".to_string(),
        session_id: "session-1".to_string(),
        command: command.to_string(),
        origin: CommandOrigin::ProviderTool,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 1,
        ended_at_ms: 10,
        duration_ms: 9,
        exit_code: 0,
        status: CommandStatus::Completed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: Some(crate::types::ShellCommandAuditIdentity {
            run_id: "run-b".to_string(),
            request_id: None,
            tool_use_id: None,
            handoff_token: Some(requests[1].token.clone()),
        }),
    };

    let ids = record_approved_shell_handoff_blocks(&mut state, &[block]);

    // The front (first) handoff stays pending: the tokened block belongs to
    // the second request and must not be mis-paired by the text fallback.
    assert!(ids.is_empty(), "{ids:?}");
    let front = state
        .control
        .shell_handoff()
        .pending_front()
        .expect("first handoff still pending");
    assert_eq!(front.request().approval_id, "req-a");
}

// #2142 review R5: the activity detail is a durable surface; the request
// preview is built from the original command text and must never carry the
// plaintext secret once a secret-bearing handoff closes normally.
#[test]
fn shell_handoff_activity_detail_redacts_the_secret_preview() {
    let mut state = InlineState::default();
    let command = r#"deploy --api-key "sk-live-secret-value""#;
    let request = ShellHandoffRequest::new(
        command,
        format!("$ {command}"),
        "approved_provider_shell_tool",
        "user",
        "req-secret",
        "run-secret",
        0,
    )
    .expect("handoff request");
    let token = request.token.clone();
    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(request);
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(0)
        .expect("emit pending handoff");
    let block = CommandBlock {
        id: "cmd-secret".to_string(),
        session_id: "session-1".to_string(),
        command: "<redacted sensitive command>".to_string(),
        origin: CommandOrigin::ProviderTool,
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 1,
        ended_at_ms: 10,
        duration_ms: 9,
        exit_code: 1,
        status: CommandStatus::Failed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: Some(crate::types::ShellCommandAuditIdentity {
            run_id: "run-secret".to_string(),
            request_id: None,
            tool_use_id: None,
            handoff_token: Some(token),
        }),
    };

    let ids = record_approved_shell_handoff_blocks(&mut state, &[block]);

    assert_eq!(ids.len(), 1, "{ids:?}");
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == ids[0])
        .expect("activity row");
    assert!(
        !row.detail.contains("sk-live-secret-value"),
        "activity detail leaked the plaintext secret: {}",
        row.detail
    );
    assert!(
        row.detail.contains("preview:"),
        "preview line disappeared entirely: {}",
        row.detail
    );
}

// The untracked closure writes the same preview into its activity row.
#[test]
fn untracked_shell_handoff_activity_detail_redacts_the_secret_preview() {
    let mut state = InlineState::default();
    let command = r#"deploy --api-key "sk-live-secret-value""#;
    let request = ShellHandoffRequest::new(
        command,
        format!("$ {command}"),
        "approved_provider_shell_tool",
        "user",
        "req-secret-untracked",
        "run-secret-untracked",
        0,
    )
    .expect("handoff request");
    state
        .control
        .shell_handoff_mut()
        .enqueue_approved_request(request);
    state
        .control
        .shell_handoff_mut()
        .emit_next_approved(0)
        .expect("emit pending handoff");
    let events = vec![untracked_test_event(ShellEventKind::ShellReady, None)];

    let ids = close_untracked_shell_handoffs(&mut state, &events);

    assert_eq!(ids.len(), 1, "{ids:?}");
    let row = state
        .activity
        .rows
        .iter()
        .find(|row| row.id == ids[0])
        .expect("activity row");
    assert!(
        !row.detail.contains("sk-live-secret-value"),
        "untracked activity detail leaked the plaintext secret: {}",
        row.detail
    );
}
