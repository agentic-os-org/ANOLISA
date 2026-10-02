use crate::adapter::{
    adapter_for_kind, AdapterKind, AgentAdapter, ClaudeCodeAdapter, FakeAgentAdapter,
    QwenCliAdapter,
};
use crate::agent::{govern_agent_events, govern_agent_events_with_language};
use crate::ledger::build_command_blocks;
use crate::parser::{
    agent_request_after_confirmation, agent_request_confirmed_by_events, findings_from_blocks,
    interventions_from_findings,
};
use crate::types::{
    AgentEvent, CommandStatus, CoshApprovalMode, FindingKind, GovernanceDecision,
    GovernancePolicyDecision, GovernedEvent, InterventionDecision, Policy, QuestionSelectionMode,
    ShellEvent, ShellEventKind,
};
use crate::ui::{render_transcript, RatatuiInlineRenderer};
use crate::Language;

fn failed_command_events() -> Vec<ShellEvent> {
    vec![
        ShellEvent::user_input_intercepted("session-1", "/explain why this failed"),
        ShellEvent::command_started("session-1", "cmd-1", "missing-command", "/work", 1000),
        ShellEvent::command_finished(
            ShellEventKind::CommandFailed,
            "session-1",
            "cmd-1",
            127,
            1037,
            "terminal://session-1/cmd-1",
        ),
    ]
}

#[test]
fn ledger_builds_failed_command_block_from_shell_events() {
    let ledger = build_command_blocks(&failed_command_events());

    assert!(ledger.errors.is_empty(), "{:?}", ledger.errors);
    assert_eq!(ledger.blocks.len(), 1);
    let block = &ledger.blocks[0];
    assert_eq!(block.id, "cmd-1");
    assert_eq!(block.command, "missing-command");
    assert_eq!(block.cwd, "/work");
    assert_eq!(block.exit_code, 127);
    assert_eq!(block.status, CommandStatus::Failed);
    assert_eq!(block.duration_ms, 37);
    assert_eq!(
        block.output.terminal_output_ref.as_deref(),
        Some("terminal://session-1/cmd-1")
    );
}

#[test]
fn parser_suggests_before_agent_request_is_created() {
    let ledger = build_command_blocks(&failed_command_events());
    let findings = findings_from_blocks(&ledger.blocks);
    let interventions = interventions_from_findings(&findings);

    assert!(findings
        .iter()
        .any(|finding| finding.kind == FindingKind::NonZeroExit));
    assert!(findings
        .iter()
        .any(|finding| finding.kind == FindingKind::CommandNotFound));
    assert!(interventions.iter().all(|intervention| {
        intervention.command_block_id == "cmd-1"
            && intervention.decision == InterventionDecision::Suggest
    }));
    assert!(
        agent_request_after_confirmation("session-1", &ledger.blocks[0], &findings, false)
            .is_none()
    );
}

#[test]
fn failed_command_does_not_confirm_agent_without_intercepted_user_request() {
    let plain_failure = vec![
        ShellEvent::command_started("session-1", "cmd-1", "missing-command", "/work", 1000),
        ShellEvent::command_finished(
            ShellEventKind::CommandFailed,
            "session-1",
            "cmd-1",
            127,
            1037,
            "terminal://session-1/cmd-1",
        ),
    ];

    assert!(!agent_request_confirmed_by_events(&plain_failure));
    assert!(agent_request_confirmed_by_events(&failed_command_events()));
}

#[test]
fn natural_language_intercept_does_not_confirm_failed_command_analysis() {
    let natural_language_then_failure = vec![
        ShellEvent::user_input_intercepted("session-1", "please explain why this failed"),
        ShellEvent::command_started("session-1", "cmd-1", "missing-command", "/work", 1000),
        ShellEvent::command_finished(
            ShellEventKind::CommandFailed,
            "session-1",
            "cmd-1",
            127,
            1037,
            "terminal://session-1/cmd-1",
        ),
    ];

    assert!(!agent_request_confirmed_by_events(
        &natural_language_then_failure
    ));
}

#[test]
fn fake_agent_and_governance_form_recommend_only_loop() {
    let ledger = build_command_blocks(&failed_command_events());
    let findings = findings_from_blocks(&ledger.blocks);
    let request = agent_request_after_confirmation("session-1", &ledger.blocks[0], &findings, true)
        .expect("confirmed request");

    let agent_events = FakeAgentAdapter.run(&request).expect("fake adapter");
    assert!(agent_events
        .iter()
        .any(|event| matches!(event, AgentEvent::TextDelta { .. })));
    assert!(!agent_events
        .iter()
        .any(|event| matches!(event, AgentEvent::Recommendation { .. })));

    let governed = govern_agent_events(&agent_events, &Policy::default());
    assert_eq!(governed.events.len(), agent_events.len());
    assert_eq!(governed.audit.len(), agent_events.len());
    assert!(governed.events.iter().all(|event| !event.auto_execute));

    let transcript = render_transcript(
        &ledger.blocks[0],
        &findings,
        &interventions_from_findings(&findings),
        &governed.events,
    );
    assert!(transcript
        .iter()
        .any(|line| line.contains("failed with exit code") || line.contains("以退出码")));
}

#[test]
fn fake_agent_streams_events_through_adapter_boundary() {
    let ledger = build_command_blocks(&failed_command_events());
    let findings = findings_from_blocks(&ledger.blocks);
    let request = agent_request_after_confirmation("session-1", &ledger.blocks[0], &findings, true)
        .expect("confirmed request");

    let mut streamed = Vec::new();
    FakeAgentAdapter
        .run_stream(&request, &mut |event| {
            streamed.push(event);
            Ok(())
        })
        .expect("fake stream adapter");

    assert!(matches!(
        streamed.first(),
        Some(AgentEvent::StatusChanged { phase, .. }) if phase == "analyzing"
    ));
    assert!(streamed
        .iter()
        .any(|event| matches!(event, AgentEvent::TextDelta { .. })));
    assert!(matches!(
        streamed.last(),
        Some(AgentEvent::AgentCompleted { .. })
    ));
}

#[test]
fn governance_outputs_executable_policy_decisions() {
    let events = vec![
        AgentEvent::StatusChanged {
            run_id: "run-1".to_string(),
            phase: "requesting".to_string(),
            message: "waiting for backend".to_string(),
        },
        AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: None,
            name: "shell".to_string(),
            input: "rm -rf /tmp/example".to_string(),
        },
        AgentEvent::Action {
            run_id: "run-1".to_string(),
            command: "sudo reboot".to_string(),
        },
        AgentEvent::Recommendation {
            run_id: "run-1".to_string(),
            summary: "Try a safer check".to_string(),
            commands: vec!["systemctl status demo".to_string()],
            auto_execute: true,
        },
        AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "tool-1".to_string(),
            stream: "stdout".to_string(),
            text: "line 1".to_string(),
        },
        AgentEvent::AgentCancelled {
            run_id: "run-1".to_string(),
            reason: "user requested cancellation".to_string(),
        },
    ];

    let governed = govern_agent_events(&events, &Policy::default());
    assert_eq!(governed.events[0].decision, GovernanceDecision::Display);
    assert_eq!(governed.events[1].decision, GovernanceDecision::Display);
    assert_eq!(
        governed.events[1].policy_decision,
        GovernancePolicyDecision::NeedsUserApproval
    );
    assert_eq!(governed.events[2].decision, GovernanceDecision::Rejected);
    assert_eq!(
        governed.events[2].policy_decision,
        GovernancePolicyDecision::HostBlocked
    );
    assert_eq!(governed.events[3].decision, GovernanceDecision::Degraded);
    assert_eq!(
        governed.events[3].policy_decision,
        GovernancePolicyDecision::DisplayOnly
    );
    assert_eq!(governed.events[4].decision, GovernanceDecision::Display);
    assert_eq!(governed.events[5].decision, GovernanceDecision::Display);
    assert!(governed.events.iter().all(|event| !event.auto_execute));
    assert!(governed.events[0]
        .display_text
        .contains("Status: requesting"));
    assert!(governed.events[1]
        .display_text
        .contains("Approval required: Bash command"));
    assert!(governed.events[1]
        .display_text
        .contains("Blocked: user approval required"));
    assert!(governed.events[2]
        .display_text
        .contains("Approval required: Shell command"));
    assert!(governed
        .events
        .iter()
        .all(|event| !event.display_text.contains("Decision: blocked by")));
    assert!(governed.events[4]
        .display_text
        .contains("Tool output: tool-1 stdout"));
    assert!(governed.events[5].display_text.contains("Agent cancelled"));
}

#[test]
fn governance_preserves_hook_notification_context() {
    let governed = govern_agent_events(
        &[AgentEvent::HookNotification {
            run_id: "run-1".to_string(),
            hook_name: "sandbox-guard".to_string(),
            message: "blocked reboot".to_string(),
            tool_use_id: Some("tool-1".to_string()),
            decision: Some("block".to_string()),
        }],
        &Policy::default(),
    );

    assert_eq!(
        governed.events[0].display_text,
        "Hook: sandbox-guard\nMessage: blocked reboot\nDecision: block"
    );
}

#[test]
fn governance_display_text_uses_requested_language_for_shell_owned_fallbacks() {
    let events = vec![
        AgentEvent::StatusChanged {
            run_id: "run-1".to_string(),
            phase: "requesting".to_string(),
            message: "waiting for approval".to_string(),
        },
        AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: None,
            name: "Bash".to_string(),
            input: "pwd".to_string(),
        },
        AgentEvent::Action {
            run_id: "run-1".to_string(),
            command: "rm -rf /tmp/demo".to_string(),
        },
        AgentEvent::Recommendation {
            run_id: "run-1".to_string(),
            summary: "建议只读检查".to_string(),
            commands: vec!["pwd".to_string()],
            auto_execute: true,
        },
        AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "tool-1".to_string(),
            stream: "stdout".to_string(),
            text: "line 1".to_string(),
        },
        AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id: "tool-1".to_string(),
            status: "failed".to_string(),
        },
        AgentEvent::UserQuestion {
            run_id: "run-1".to_string(),
            provider_request_id: None,
            question: "继续吗？".to_string(),
            options: vec!["继续".to_string(), "停止".to_string()],
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
        },
        AgentEvent::UserQuestion {
            run_id: "run-1".to_string(),
            provider_request_id: None,
            question: String::new(),
            options: Vec::new(),
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
        },
        AgentEvent::AgentCancelled {
            run_id: "run-1".to_string(),
            reason: "user requested cancellation".to_string(),
        },
        AgentEvent::AgentFailed {
            run_id: "run-1".to_string(),
            error: "analysis returned an error".to_string(),
            error_code: None,
            max_turns: None,
        },
        AgentEvent::HookNotification {
            run_id: "run-1".to_string(),
            hook_name: String::new(),
            message: "  ".to_string(),
            tool_use_id: Some("tool-1".to_string()),
            decision: None,
        },
    ];

    let governed = govern_agent_events_with_language(&events, &Policy::default(), Language::ZhCn);
    let text = governed
        .events
        .iter()
        .map(|event| event.display_text.as_str())
        .collect::<Vec<_>>()
        .join("\n");

    assert!(text.contains("状态: requesting"), "{text}");
    assert!(text.contains("需要审批: Bash 命令"), "{text}");
    assert!(text.contains("需要审批: Shell 命令"), "{text}");
    assert!(text.contains("已阻止: 需要用户审批"), "{text}");
    assert!(text.contains("推荐命令:"), "{text}");
    assert!(text.contains("Tool 输出: tool-1 stdout"), "{text}");
    assert!(text.contains("Tool 已完成: tool-1"), "{text}");
    assert!(text.contains("问题: 继续吗？"), "{text}");
    assert!(text.contains("问题: Agent 需要你的输入"), "{text}");
    assert!(text.contains("Agent 已取消"), "{text}");
    assert!(text.contains("原因: 用户请求取消"), "{text}");
    assert!(text.contains("分析返回错误"), "{text}");
    assert!(text.contains("Hook: 未知 Hook"), "{text}");
    assert!(text.contains("消息: 未提供消息"), "{text}");
    assert!(text.contains("决策: 未指定"), "{text}");
    assert!(!text.contains("Approval required:"), "{text}");
    assert!(!text.contains("analysis returned an error"), "{text}");
    assert!(!text.contains("Blocked: user approval required"), "{text}");
    assert!(!text.contains("recommended commands:"), "{text}");
    assert!(!text.contains("Tool output:"), "{text}");
    assert!(!text.contains("Question: Agent needs your input"), "{text}");
    assert!(!text.contains("unknown hook"), "{text}");
    assert!(!text.contains("no message provided"), "{text}");
    assert!(!text.contains("unspecified"), "{text}");
}

#[test]
fn cli_adapters_prepare_safe_non_intrusive_invocations() {
    let ledger = build_command_blocks(&failed_command_events());
    let findings = findings_from_blocks(&ledger.blocks);
    let request = agent_request_after_confirmation("session-1", &ledger.blocks[0], &findings, true)
        .expect("confirmed request");

    let claude =
        ClaudeCodeAdapter::default().prepare_invocation(&request, CoshApprovalMode::Recommend);
    assert_eq!(claude.program, "claude");
    assert!(claude.args.contains(&"--print".to_string()));
    assert!(claude.args.contains(&"--output-format".to_string()));
    assert!(claude.args.contains(&"stream-json".to_string()));
    assert!(claude.args.contains(&"--verbose".to_string()));
    assert!(claude
        .args
        .contains(&"--include-partial-messages".to_string()));
    assert!(claude.args.contains(&"--permission-mode".to_string()));
    assert!(claude.args.contains(&"plan".to_string()));
    assert!(claude.args.contains(&"--tools".to_string()));
    assert!(claude.args.contains(&"default".to_string()));
    assert!(!claude.args.contains(&"--allowedTools".to_string()));
    assert!(!claude
        .args
        .contains(&"--no-session-persistence".to_string()));
    assert!(!claude
        .args
        .contains(&"--dangerously-skip-permissions".to_string()));
    assert!(claude
        .prompt
        .contains("approval system that reviews every tool request"));
    let qwen = QwenCliAdapter::default().prepare_invocation(&request, CoshApprovalMode::Recommend);
    assert_eq!(qwen.program, "co");
    assert!(qwen.args.contains(&"--approval-mode".to_string()));
    assert!(qwen.args.contains(&"plan".to_string()));
    assert!(qwen.args.contains(&"--input-format".to_string()));
}

#[test]
fn natural_language_prompt_guides_tool_and_question_intents() {
    let ledger = build_command_blocks(&failed_command_events());
    let findings = findings_from_blocks(&ledger.blocks);
    let mut request =
        agent_request_after_confirmation("session-1", &ledger.blocks[0], &findings, true)
            .expect("confirmed request");
    request.user_input = Some("执行 ps aux 看一下".to_string());

    let claude =
        ClaudeCodeAdapter::default().prepare_invocation(&request, CoshApprovalMode::default());

    assert!(claude.prompt.contains("use the Bash tool directly"));
    assert!(claude.prompt.contains("request AskUserQuestion"));
    assert!(claude.prompt.contains("Decide based on user intent:"));
}

#[test]
fn continuation_prompts_do_not_reenter_generic_shell_request_mode() {
    let ledger = build_command_blocks(&failed_command_events());
    let findings = findings_from_blocks(&ledger.blocks);
    let mut request =
        agent_request_after_confirmation("session-1", &ledger.blocks[0], &findings, true)
            .expect("confirmed request");

    request.user_input =
        Some("Answer to pending Agent question: 你喜欢什么颜色？\nUser answer: 白色".to_string());
    let question_prompt = ClaudeCodeAdapter::default()
        .prepare_invocation(&request, CoshApprovalMode::default())
        .prompt;
    assert!(question_prompt.contains("Continue the same Shell-first Agent session"));
    assert!(question_prompt.contains("Do not ask the same question again"));
    assert!(!question_prompt.contains("Return explanation and recommended next commands only"));

    request.user_input = Some(
        "Tool result for approved request req-1\nTool: tool Bash\nCommand: pwd\nStatus: executed\nExit code: 0\nReason: ok\nStdout:\n/work\nStderr:\n".to_string(),
    );
    let tool_prompt = ClaudeCodeAdapter::default()
        .prepare_invocation(&request, CoshApprovalMode::default())
        .prompt;
    assert!(tool_prompt.contains("this tool result"));
    assert!(tool_prompt.contains("Analyze only the result below"));
    assert!(tool_prompt.contains("pre-approval prose"));
    assert!(tool_prompt.contains("do not continue an earlier recommendation list"));
    assert!(!tool_prompt.contains("Return explanation and recommended next commands only"));

    request.user_input = Some(
        "Approval result for request req-1\nTool: tool Bash\nCommand: pwd\nDecision: denied by user\nStatus: not_executed\nNo command ran.".to_string(),
    );
    let approval_prompt = ClaudeCodeAdapter::default()
        .prepare_invocation(&request, CoshApprovalMode::default())
        .prompt;
    assert!(approval_prompt.contains("approval decision"));
    assert!(approval_prompt.contains("No shell command ran"));
    assert!(!approval_prompt.contains("Return explanation and recommended next commands only"));
}

#[test]
fn fake_and_qwen_use_same_agent_adapter_boundary() {
    let ledger = build_command_blocks(&failed_command_events());
    let findings = findings_from_blocks(&ledger.blocks);
    let request = agent_request_after_confirmation("session-1", &ledger.blocks[0], &findings, true)
        .expect("confirmed request");

    let fake_events = adapter_for_kind(AdapterKind::Fake)
        .run(&request)
        .expect("fake adapter");
    let qwen_events = adapter_for_kind(AdapterKind::QwenCli)
        .run(&request)
        .expect("qwen adapter");

    assert!(fake_events
        .iter()
        .any(|event| matches!(event, AgentEvent::TextDelta { .. })));
    assert!(!fake_events
        .iter()
        .any(|event| matches!(event, AgentEvent::Recommendation { .. })));
    assert!(qwen_events.iter().any(|event| matches!(
        event,
        AgentEvent::TextDelta { text, .. }
            if text.contains("--approval-mode plan") && text.contains("co adapter")
    )));

    let governed = govern_agent_events(&qwen_events, &Policy::default());
    assert_eq!(governed.events.len(), qwen_events.len());
    assert_eq!(governed.audit.len(), qwen_events.len());
    assert!(governed.events.iter().all(|event| !event.auto_execute));
}

#[test]
fn fake_bound_insight_ignores_free_form_fixture_keywords() {
    let ledger = build_command_blocks(&failed_command_events());
    let findings = findings_from_blocks(&ledger.blocks);
    let mut request =
        agent_request_after_confirmation("session-1", &ledger.blocks[0], &findings, true)
            .expect("confirmed request");
    request.user_input = Some("tool recommendation fixture".to_string());
    request
        .context_hints
        .push("__cosh_request_source=insight_prompt".to_string());

    let events = FakeAgentAdapter.run(&request).expect("fake adapter");

    assert!(events
        .iter()
        .any(|event| matches!(event, AgentEvent::TextDelta { .. })));
    assert!(!events.iter().any(|event| matches!(
        event,
        AgentEvent::ToolCall { .. } | AgentEvent::Recommendation { .. } | AgentEvent::Action { .. }
    )));
}

#[test]
fn adapter_capabilities_are_provider_neutral() {
    let fake = adapter_for_kind(AdapterKind::Fake).capabilities();
    let claude = adapter_for_kind(AdapterKind::ClaudeCode).capabilities();
    let qwen = adapter_for_kind(AdapterKind::QwenCli).capabilities();

    assert!(fake.text_stream);
    assert!(fake.tool_intent);
    assert!(fake.user_question);
    assert!(!fake.session_resume);

    assert!(claude.text_stream);
    assert!(claude.thinking_stream);
    assert!(claude.session_resume);
    assert!(claude.tool_intent);
    assert!(claude.user_question);
    assert!(claude.cancellable);

    assert!(qwen.session_resume);
    assert!(qwen.tool_intent);
    assert!(qwen.user_question);
}

#[test]
fn claude_code_adapter_is_dry_run_by_default_and_governed() {
    let ledger = build_command_blocks(&failed_command_events());
    let findings = findings_from_blocks(&ledger.blocks);
    let request = agent_request_after_confirmation("session-1", &ledger.blocks[0], &findings, true)
        .expect("confirmed request");

    let claude_events = adapter_for_kind(AdapterKind::ClaudeCode)
        .run(&request)
        .expect("claude adapter");

    assert!(claude_events.iter().any(|event| matches!(
        event,
        AgentEvent::TextDelta { text, .. }
            if text.contains("--print") && text.contains("--permission-mode plan") && text.contains("claude")
    )));
    assert!(claude_events.iter().any(|event| matches!(
        event,
        AgentEvent::AgentCompleted { summary, .. }
            if summary.contains("dry-run completed without model call")
    )));

    let governed = govern_agent_events(&claude_events, &Policy::default());
    assert_eq!(governed.events.len(), claude_events.len());
    assert_eq!(governed.audit.len(), claude_events.len());
    assert!(governed.events.iter().all(|event| !event.auto_execute));
}

#[test]
fn write_governed_events_skips_empty_display_text() {
    // F3 (#2067): a batch whose events carry no visible content must not
    // render an empty titled card. Lives here instead of next to the
    // renderer because the whole ui tree is shared between the lib shim and
    // the bin target, and new tests must not grow the lib/bin overlap.
    let event = GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::HookNotification {
            run_id: "run-1".to_string(),
            hook_name: "guard".to_string(),
            message: String::new(),
            tool_use_id: Some("toolu-1".to_string()),
            decision: None,
        },
        reason: "orphan".to_string(),
        display_text: String::new(),
        auto_execute: false,
    };
    let renderer = RatatuiInlineRenderer::with_width(80);
    let mut output = Vec::new();

    renderer
        .write_governed_events(&mut output, &[event])
        .unwrap();

    assert!(
        output.is_empty(),
        "an empty display text must not render a card: {}",
        String::from_utf8_lossy(&output)
    );
}
