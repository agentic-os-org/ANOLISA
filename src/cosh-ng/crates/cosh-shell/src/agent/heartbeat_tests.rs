//! Tests for the Agent heartbeat and its status surfaces.
//!
//! Kept beside `heartbeat.rs` rather than inline: the production module is
//! close to the 1000-line ceiling and this crate already pairs a module with a
//! `*_tests.rs` sibling.

use super::*;

fn test_active_run() -> ActiveAgentRun {
    let request = AgentRequest {
        id: "request-1".to_string(),
        session_id: "session-1".to_string(),
        command_block: CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: "hello".to_string(),
            origin: Default::default(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            duration_ms: 1,
            exit_code: 0,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some("hello".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let handle = adapter.start_cancellable(request.clone(), CoshApprovalMode::Recommend);
    let renderer = RatatuiInlineRenderer::for_terminal();
    ActiveAgentRun {
        request,
        origin: AgentRunOrigin::Standard,
        handle,
        provider_name: "fake",
        language: Language::EnUs,
        renderer: renderer.clone(),
        status_animation: renderer.status_animation(),
        markdown_stream: renderer.stream_markdown_agent(),
        governed_events: Vec::new(),
        deferred_events: Vec::new(),
        held_events: Vec::new(),
        cosh_request_filter: crate::evidence::stream::CoshRequestStreamFilter::default(),
        pending_cosh_requests: Vec::new(),
        pending_cosh_request_audits: Vec::new(),
        rendered_governed_event_count: 0,
        selectable_after_event_index: None,
        started_at: Instant::now(),
        last_activity_at: Instant::now(),
        last_heartbeat_at: Instant::now(),
        current_phase: String::new(),
        current_message: String::new(),
        has_visible_text_delta: false,
        completed: false,
        host_completed_tool_ids: Vec::new(),
        pending_hook_notifications: Vec::new(),
    }
}

#[test]
fn tool_call_activity_is_not_reported_as_waiting_for_approval() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    remember_agent_activity(
        &mut active_run,
        &[GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::NeedsUserApproval,
            event: AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("tool-1".to_string()),
                name: "glob".to_string(),
                input: r#"{"pattern":"**/README.md"}"#.to_string(),
            },
            reason: "provider tool call visible".to_string(),
            display_text: "provider tool call visible".to_string(),
            auto_execute: false,
        }],
    );

    assert_eq!(active_run.current_phase, "tool");
    assert!(active_run.current_message.contains("正在查找文件 1 项"));
    assert!(!active_run.current_message.contains("approval"));
    assert!(!active_run.current_message.contains("README.md"));
}

#[test]
fn pending_tool_status_aggregates_by_kind_and_removes_completed_tool() {
    let events = vec![
        GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("read-1".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"/very/long/private/path/a.md"}"#.to_string(),
            },
            reason: "read".to_string(),
            display_text: String::new(),
            auto_execute: false,
        },
        GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("read-2".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"/very/long/private/path/b.md"}"#.to_string(),
            },
            reason: "read".to_string(),
            display_text: String::new(),
            auto_execute: false,
        },
        GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("grep-1".to_string()),
                name: "Grep".to_string(),
                input: r#"{"path":"src","query":"needle"}"#.to_string(),
            },
            reason: "grep".to_string(),
            display_text: String::new(),
            auto_execute: false,
        },
    ];

    let summary =
        pending_tool_status_detail(Language::ZhCn, events.iter()).expect("pending summary");
    assert!(summary.contains("正在读取 2 个文件"), "{summary}");
    assert!(summary.contains("正在搜索 1 项"), "{summary}");
    assert!(!summary.contains("private"), "{summary}");
    assert!(!summary.contains("needle"), "{summary}");

    let mut completed_events = events;
    completed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id: "read-1".to_string(),
            status: "success".to_string(),
        },
        reason: "done".to_string(),
        display_text: String::new(),
        auto_execute: false,
    });

    let summary = pending_tool_status_detail(Language::ZhCn, completed_events.iter())
        .expect("pending summary");
    assert!(summary.contains("正在读取 1 个文件"), "{summary}");
    assert!(summary.contains("正在搜索 1 项"), "{summary}");

    completed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id: "read-2".to_string(),
            status: "success".to_string(),
        },
        reason: "done".to_string(),
        display_text: String::new(),
        auto_execute: false,
    });
    completed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id: "grep-1".to_string(),
            status: "success".to_string(),
        },
        reason: "done".to_string(),
        display_text: String::new(),
        auto_execute: false,
    });

    assert!(pending_tool_status_detail(Language::ZhCn, completed_events.iter()).is_none());
}

#[test]
fn pending_tool_status_finishes_open_agent_response() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    let renderer = RatatuiInlineRenderer::with_width(100);
    active_run.renderer = renderer.clone();
    active_run.status_animation = renderer.status_animation();
    active_run.markdown_stream = renderer.stream_markdown_agent();
    let mut output = Vec::new();

    active_run
        .markdown_stream
        .write_delta(&mut output, "Agent text before tool.\n\n")
        .expect("write agent text");
    active_run.has_visible_text_delta = true;
    active_run.governed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("read-1".to_string()),
            name: "Read".to_string(),
            input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
        },
        reason: "read".to_string(),
        display_text: String::new(),
        auto_execute: false,
    });

    render_agent_pending_tool_status(&mut active_run, &mut output).expect("render status");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!active_run.markdown_stream.has_started());
    assert!(output.contains("正在读取 1 个文件"), "{output}");
    assert!(output.contains('╰'), "{output}");
}

#[test]
fn pending_tool_status_flushes_buffered_agent_response() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    let renderer = RatatuiInlineRenderer::with_width(100);
    active_run.renderer = renderer.clone();
    active_run.status_animation = renderer.status_animation();
    active_run.markdown_stream = renderer.stream_markdown_agent();
    let mut output = Vec::new();

    active_run
        .markdown_stream
        .write_delta(&mut output, "Agent text before tool")
        .expect("write buffered agent text");
    active_run.has_visible_text_delta = true;
    assert!(!active_run.markdown_stream.has_started());
    assert!(active_run.markdown_stream.has_buffered_text());
    active_run.governed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("write-1".to_string()),
            name: "Write".to_string(),
            input: r#"{"file_path":"snake.py"}"#.to_string(),
        },
        reason: "write".to_string(),
        display_text: String::new(),
        auto_execute: false,
    });

    render_agent_pending_tool_status(&mut active_run, &mut output).expect("render status");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!active_run.markdown_stream.has_started());
    assert!(!active_run.markdown_stream.has_buffered_text());
    assert!(output.contains("Agent text before tool"), "{output}");
    assert!(output.contains("正在写入 1 项"), "{output}");
    assert!(output.contains('╰'), "{output}");
}

#[test]
fn shell_evidence_pending_status_finishes_open_agent_response() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    let renderer = RatatuiInlineRenderer::with_width(100).with_language(Language::ZhCn);
    active_run.markdown_stream = renderer.stream_markdown_agent();
    let mut output = Vec::new();

    active_run
        .markdown_stream
        .write_delta(&mut output, "先看一下证据。\n\n")
        .expect("write agent text");
    active_run.has_visible_text_delta = true;
    render_agent_shell_evidence_pending_status(&mut active_run, &mut output)
        .expect("render shell evidence status");
    let output = String::from_utf8(output).expect("utf8 output");

    assert!(!active_run.markdown_stream.has_started());
    assert!(output.contains("Shell 证据 1 项"), "{output}");
    assert!(output.contains('╰'), "{output}");
}

#[test]
fn host_completed_shell_tools_are_removed_from_pending_status() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    active_run.governed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("shell-1".to_string()),
            name: "Bash".to_string(),
            input: r#"{"command":"echo one"}"#.to_string(),
        },
        reason: "shell".to_string(),
        display_text: String::new(),
        auto_execute: false,
    });
    active_run.mark_host_completed_tool("shell-1");
    active_run.governed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("shell-2".to_string()),
            name: "Bash".to_string(),
            input: r#"{"command":"echo two"}"#.to_string(),
        },
        reason: "shell".to_string(),
        display_text: String::new(),
        auto_execute: false,
    });

    let summary = pending_tool_status_detail_for_run(&active_run).expect("pending shell");
    assert!(summary.contains("正在执行 Shell 1 项"), "{summary}");
    assert!(!summary.contains("2 项"), "{summary}");

    active_run.mark_host_completed_tool("shell-2");
    active_run.governed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("shell-3".to_string()),
            name: "Bash".to_string(),
            input: r#"{"command":"echo three"}"#.to_string(),
        },
        reason: "shell".to_string(),
        display_text: String::new(),
        auto_execute: false,
    });

    let summary = pending_tool_status_detail_for_run(&active_run).expect("pending shell");
    assert!(summary.contains("正在执行 Shell 1 项"), "{summary}");
    assert!(!summary.contains("3 项"), "{summary}");
}

#[test]
fn pending_tool_status_deduplicates_matching_permission_request() {
    let events = [
        GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("toolu-write".to_string()),
                name: "Write".to_string(),
                input: r#"{"file_path":"/tmp/report.md","content":"secret body"}"#.to_string(),
            },
            reason: "write".to_string(),
            display_text: String::new(),
            auto_execute: false,
        },
        GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::NeedsUserApproval,
            event: AgentEvent::ToolPermissionRequest {
                run_id: "run-1".to_string(),
                request_id: "ctrl-write".to_string(),
                tool_name: "Write".to_string(),
                tool_input: serde_json::json!({
                    "file_path": "/tmp/report.md",
                    "content": "secret body"
                }),
                tool_use_id: "toolu-write".to_string(),
                hook_requires_approval: false,
                audit_ref: None,
            },
            reason: "approval".to_string(),
            display_text: String::new(),
            auto_execute: false,
        },
    ];

    let summary =
        pending_tool_status_detail(Language::ZhCn, events.iter()).expect("pending summary");
    assert!(summary.contains("正在写入 1 项"), "{summary}");
    assert!(!summary.contains("正在写入 2 项"), "{summary}");
    assert!(!summary.contains("secret body"), "{summary}");
    assert!(!summary.contains("/tmp/report.md"), "{summary}");
}

#[test]
fn pending_tool_status_uses_request_id_when_tool_use_id_is_empty() {
    let events = [
        GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::NeedsUserApproval,
            event: AgentEvent::ToolPermissionRequest {
                run_id: "run-1".to_string(),
                request_id: "ctrl-read-1".to_string(),
                tool_name: "Read".to_string(),
                tool_input: serde_json::json!({ "file_path": "a.md" }),
                tool_use_id: String::new(),
                hook_requires_approval: false,
                audit_ref: None,
            },
            reason: "approval".to_string(),
            display_text: String::new(),
            auto_execute: false,
        },
        GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::NeedsUserApproval,
            event: AgentEvent::ToolPermissionRequest {
                run_id: "run-1".to_string(),
                request_id: "ctrl-read-2".to_string(),
                tool_name: "Read".to_string(),
                tool_input: serde_json::json!({ "file_path": "b.md" }),
                tool_use_id: String::new(),
                hook_requires_approval: false,
                audit_ref: None,
            },
            reason: "approval".to_string(),
            display_text: String::new(),
            auto_execute: false,
        },
    ];

    let summary =
        pending_tool_status_detail(Language::ZhCn, events.iter()).expect("pending summary");
    assert!(summary.contains("正在读取 2 个文件"), "{summary}");
    assert!(!summary.contains("ctrl-read"), "{summary}");
}

#[test]
fn question_activity_localizes_empty_question_fallback() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    remember_agent_activity(
        &mut active_run,
        &[GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::UserQuestion {
                run_id: "run-1".to_string(),
                provider_request_id: None,
                question: String::new(),
                options: Vec::new(),
                allow_free_text: true,
                selection_mode: QuestionSelectionMode::Single,
            },
            reason: "agent question requires explicit user input".to_string(),
            display_text: String::new(),
            auto_execute: false,
        }],
    );

    assert_eq!(active_run.current_phase, "问题");
    assert!(active_run.current_message.contains("Agent 需要你的输入"));
    assert!(!active_run
        .current_message
        .contains("Agent needs your input"));
}

#[test]
fn provider_status_activity_localizes_neutral_tokens() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    active_run.started_at = Instant::now() - Duration::from_secs(60);
    active_run.last_activity_at = Instant::now() - Duration::from_secs(30);
    remember_agent_activity(
        &mut active_run,
        &[GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::StatusChanged {
                run_id: "run-1".to_string(),
                phase: "thinking".to_string(),
                message: "thinking".to_string(),
            },
            reason: "agent status is display-only".to_string(),
            display_text: String::new(),
            auto_execute: false,
        }],
    );

    assert_eq!(active_run.current_phase, "正在思考");
    assert_eq!(active_run.current_message, "正在思考");
    assert!(
        active_run.last_activity_at.elapsed() < Duration::from_secs(1),
        "status progress must refresh the provider idle clock"
    );
}

#[test]
fn tool_argument_status_survives_streamed_text_and_ends_its_surface() {
    let mut active_run = test_active_run();
    active_run.language = Language::EnUs;
    let mut output = Vec::new();

    // Text streamed first: without the fallback both the markdown surface and
    // the "no pending tool" path swallow the argument-generation status.
    active_run
        .markdown_stream
        .write_delta(&mut output, "Let me write that file.\n\n")
        .expect("stream text");
    active_run.has_visible_text_delta = true;
    assert!(
        active_run.markdown_stream.has_started(),
        "the test needs a live text surface to defeat"
    );

    remember_agent_activity(
        &mut active_run,
        &[GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::StatusChanged {
                run_id: "run-1".to_string(),
                phase: "tool_arguments".to_string(),
                message: "generating tool arguments: write_file".to_string(),
            },
            reason: "agent status is display-only".to_string(),
            display_text: String::new(),
            auto_execute: false,
        }],
    );

    assert_eq!(
        status_detail_for_run(&active_run).as_deref(),
        Some("generating write_file arguments...")
    );

    output.clear();
    render_agent_pending_tool_status(&mut active_run, &mut output).expect("render status");
    let rendered = String::from_utf8(output).expect("utf8 terminal output");
    assert!(
        rendered.contains("generating write_file arguments"),
        "{rendered:?}"
    );
    // The text surface was closed, so the heartbeat can keep animating it.
    assert!(!active_run.markdown_stream.has_started());
    assert!(!active_run.has_visible_text_delta);
}

#[test]
fn tool_argument_generation_status_is_localized_and_names_only_the_tool() {
    for (language, expected_phase, expected_message) in [
        (Language::ZhCn, "工具参数", "正在生成 write_file 参数..."),
        (
            Language::EnUs,
            "tool arguments",
            "generating write_file arguments...",
        ),
    ] {
        let mut active_run = test_active_run();
        active_run.language = language;
        remember_agent_activity(
            &mut active_run,
            &[GovernedEvent {
                decision: GovernanceDecision::Display,
                policy_decision: GovernancePolicyDecision::DisplayOnly,
                event: AgentEvent::StatusChanged {
                    run_id: "run-1".to_string(),
                    phase: "tool_arguments".to_string(),
                    message: "generating tool arguments: write_file".to_string(),
                },
                reason: "agent status is display-only".to_string(),
                display_text: String::new(),
                auto_execute: false,
            }],
        );

        assert_eq!(active_run.current_phase, expected_phase);
        assert_eq!(active_run.current_message, expected_message);
    }
}

#[test]
fn pending_tool_status_takes_precedence_over_argument_generation() {
    let mut active_run = test_active_run();
    active_run.language = Language::EnUs;
    active_run.current_phase = I18n::new(Language::EnUs)
        .t(MessageId::AgentStatusToolArguments)
        .to_string();
    active_run.current_message = "generating write_file arguments...".to_string();
    active_run.governed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("tool-1".to_string()),
            name: "shell".to_string(),
            input: r#"{"command":"sleep 5"}"#.to_string(),
        },
        reason: "provider tool call visible".to_string(),
        display_text: String::new(),
        auto_execute: false,
    });

    assert_eq!(
        status_detail_for_run(&active_run).as_deref(),
        Some("running shell command: 1"),
        "a materialized pending tool must not be hidden by a later status token"
    );
}

#[test]
fn elapsed_heartbeat_does_not_repeat_generic_thinking_detail() {
    let zh = I18n::new(Language::ZhCn);
    assert_eq!(
        elapsed_thinking_text(&zh, Language::ZhCn, 7, "正在思考"),
        "正在思考... 7s"
    );
    assert_eq!(
        elapsed_thinking_text(&zh, Language::ZhCn, 7, "正在读取 1 个文件"),
        "正在思考... 7s · 正在读取 1 个文件"
    );

    let en = I18n::new(Language::EnUs);
    assert_eq!(
        elapsed_thinking_text(&en, Language::EnUs, 7, "thinking"),
        "Thinking... 7s"
    );
}

#[test]
fn shell_evidence_activity_uses_concise_tool_status() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    remember_agent_activity(
        &mut active_run,
        &[GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::ShellEvidenceRequest {
                run_id: "run-1".to_string(),
                request_id: "evidence-1".to_string(),
                tool_use_id: "toolu-evidence".to_string(),
                action: crate::adapter::ShellEvidenceAction::ListCommands {
                    limit: 5,
                    cursor: None,
                },
            },
            reason: "shell evidence".to_string(),
            display_text: String::new(),
            auto_execute: false,
        }],
    );

    assert_eq!(active_run.current_phase, "tool");
    assert_eq!(active_run.current_message, "正在处理 Shell 证据 1 项");
    assert!(!active_run.current_message.contains("list_commands"));
    assert!(!active_run.current_message.contains("toolu-evidence"));
}

#[test]
fn agent_completion_activity_localizes_neutral_summary() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    remember_agent_activity(
        &mut active_run,
        &[GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::AgentCompleted {
                run_id: "run-1".to_string(),
                summary: "analysis completed".to_string(),
            },
            reason: "agent completion is display-only".to_string(),
            display_text: String::new(),
            auto_execute: false,
        }],
    );

    assert_eq!(active_run.current_phase, "已完成");
    assert_eq!(active_run.current_message, "分析完成");
}

#[test]
fn provider_startup_activity_hides_adapter_name_in_zh() {
    let mut active_run = test_active_run();
    active_run.language = Language::ZhCn;
    remember_agent_activity(
        &mut active_run,
        &[GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::StatusChanged {
                run_id: "run-1".to_string(),
                phase: "starting".to_string(),
                message: "starting cosh-tui headless backend".to_string(),
            },
            reason: "agent status is display-only".to_string(),
            display_text: String::new(),
            auto_execute: false,
        }],
    );

    assert_eq!(active_run.current_message, "正在启动模型后端");
    assert!(!active_run.current_message.contains("cosh-tui"));
}
