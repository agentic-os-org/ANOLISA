use super::approval_bridge::*;
use crate::runtime::evidence_delivery::record_readonly_compound_completion;
use crate::runtime::prelude::*;
use crate::tools::{ReadonlyPipelineError, ReadonlyPipelineOutput};

fn analysis_only_request() -> AgentRequest {
    AgentRequest {
        id: "agent-request-1".to_string(),
        session_id: "session-1".to_string(),
        command_block: CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: "ShellCommandCompleted evidence".to_string(),
            origin: Default::default(),
            cwd: "/tmp".to_string(),
            end_cwd: "/tmp".to_string(),
            started_at_ms: 0,
            ended_at_ms: 0,
            duration_ms: 0,
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
        context_hints: vec!["analysis-only continuation after foreground shell handoff".to_string()],
        user_input: Some("ShellCommandCompleted evidence".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

/// A plain provider run request (no analysis-continuation hints):
/// executor-route tests need the request's `command_block.cwd` to
/// reach the executor without tripping the analysis-only shell
/// denial policy.
fn compound_run_request() -> AgentRequest {
    let mut request = analysis_only_request();
    request.context_hints = Vec::new();
    request.user_input = Some("check disk usage".to_string());
    request
}

fn compound_tool_call_event(command: &str) -> GovernedEvent {
    GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: None,
            name: "run_shell_command".to_string(),
            input: serde_json::json!({"command": command}).to_string(),
        },
        reason: "visible streamed tool call".to_string(),
        display_text: "visible streamed tool call".to_string(),
        auto_execute: false,
    }
}

#[test]
fn auto_mode_runs_fully_readonly_compound_through_executor() {
    let dir = tempfile::tempdir().expect("isolated execution cwd");
    std::fs::write(dir.path().join("cosh-fixture"), "fixture").expect("write fixture");
    let cwd = dir.path().display().to_string();
    for command in [
        "pwd && df -h",
        "find . -maxdepth 1 -name '*cosh*' 2>\"/dev/null\"",
    ] {
        // Both grants must execute and produce completion evidence without
        // queuing a shell handoff or leaving a pending approval card.
        let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
        let mut state = InlineState {
            approval_mode: CoshApprovalMode::Auto,
            ..InlineState::default()
        };
        let mut run_request = compound_run_request();
        run_request.command_block.cwd = cwd.clone();
        run_request.command_block.end_cwd = cwd.clone();
        let mut output = Vec::new();

        let handled = render_auto_approved_tool(
            &mut state,
            &[compound_tool_call_event(command)],
            Some(&run_request),
            AgentRunOrigin::Standard,
            &mut output,
            &adapter,
        )
        .expect("render auto approval");

        assert!(handled);
        assert!(state.control.shell_handoff().approved_is_empty());
        let evidence = state
            .evidence
            .latest_shell_command_completed()
            .expect("executor completion evidence");
        assert_eq!(evidence.command, command);
        assert_eq!(evidence.exit_code, 0, "readonly plan must really execute");
        assert_eq!(evidence.status, "completed");
        // R5: the executor runs in — and the evidence reports — the
        // requesting shell's directory, not this process's cwd.
        assert_eq!(evidence.cwd, cwd);
        assert_eq!(evidence.end_cwd, cwd);
        assert!(!evidence.provider_result_delivered);
        assert_eq!(
            evidence.provider_result_delivery_status,
            "not_provider_tool_request"
        );
        assert_eq!(state.approvals.requests.len(), 1);
        assert_eq!(
            state.approvals.requests[0].status,
            ApprovalRequestStatus::Approved
        );
    }
}

#[test]
fn audit_required_failure_blocks_compound_executor() {
    for command in ["pwd && df -h", "ls 2>/dev/null"] {
        // R5 P1 regression: when the required audit writer is
        // unavailable, record_auto_approved_request marks the request
        // Blocked; the executor route must honor that terminal state —
        // nothing executes and no completion evidence is recorded.
        let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
        let mut state = InlineState {
            approval_mode: CoshApprovalMode::Auto,
            audit: Some(
                crate::journal::audit::ShellAuditRecorder::test_required_unavailable("session-1"),
            ),
            ..InlineState::default()
        };
        let run_request = compound_run_request();
        let mut output = Vec::new();

        let handled = render_auto_approved_tool(
            &mut state,
            &[compound_tool_call_event(command)],
            Some(&run_request),
            AgentRunOrigin::Standard,
            &mut output,
            &adapter,
        )
        .expect("render auto approval");

        assert!(handled);
        assert!(
            state.evidence.latest_shell_command_completed().is_none(),
            "blocked request must never reach the executor"
        );
        assert_eq!(state.approvals.requests.len(), 1);
        assert_eq!(
            state.approvals.requests[0].status,
            ApprovalRequestStatus::Blocked
        );
        assert_eq!(
            state.approvals.requests[0].execution_path,
            Some("blocked_audit_required")
        );
    }
}

#[test]
fn compound_completion_status_mirrors_handoff_outcomes() {
    // R6 P1 regression: the executor route reports the same status
    // vocabulary as the handoff path — never "completed" for a
    // failed, timed out, or unstarted execution.
    let finished = |code: i32| {
        Ok(ReadonlyPipelineOutput {
            exit_code: Some(code),
            stdout: String::new(),
            stderr: String::new(),
        })
    };
    assert_eq!(
        readonly_compound_completion_status(&finished(0), "pwd && df -h"),
        "completed"
    );
    assert_eq!(
        readonly_compound_completion_status(&finished(2), "ls missing && pwd"),
        "failed"
    );
    assert_eq!(
        readonly_compound_completion_status(&finished(127), "pwd && df -h"),
        "failed"
    );
    assert_eq!(
        readonly_compound_completion_status(&finished(130), "pwd && df -h"),
        "interrupted"
    );
    assert_eq!(
        readonly_compound_completion_status(
            &Err(ReadonlyPipelineError {
                reason: "stage-timeout",
                detail: "sleep 2".to_string(),
            }),
            "pwd && df -h"
        ),
        "timed_out"
    );
    assert_eq!(
        readonly_compound_completion_status(
            &Err(ReadonlyPipelineError {
                reason: "executor-spawn",
                detail: "denied".to_string(),
            }),
            "pwd && df -h"
        ),
        "not_executed"
    );
}

#[test]
fn auto_mode_reports_failed_status_for_nonzero_compound() {
    // R6 P1 regression, production chain: an eligible compound
    // whose first segment exits non-zero must surface as failed —
    // not completed — in the provider-visible evidence.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    let run_request = compound_run_request();
    let mut output = Vec::new();

    let handled = render_auto_approved_tool(
        &mut state,
        &[compound_tool_call_event(
            "ls /cosh-1882-definitely-missing-dir && pwd",
        )],
        Some(&run_request),
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert!(handled);
    let evidence = state
        .evidence
        .latest_shell_command_completed()
        .expect("executor completion evidence");
    assert_eq!(evidence.status, "failed");
    assert_ne!(evidence.exit_code, 0);
}

#[test]
fn compound_completion_scrubs_control_split_secrets() {
    // R5 P1 regression: executor output bypasses the PTY capture
    // pipeline, so the completion must run the canonical control-
    // sequence cleaner BEFORE redaction — otherwise a NUL-split
    // secret (`api_\0key=...`) never matches the redaction patterns
    // yet renders as an ordinary assignment.
    let mut state = InlineState::default();
    let request = shell_request(ProviderShellRequestKind::LocalApproval, None, None);
    let output = crate::tools::readonly_pipeline::ReadonlyPipelineOutput {
        exit_code: Some(0),
        stdout: "api_\u{0}key=abcdef1234567890abcdef\n".to_string(),
        stderr: String::new(),
    };
    let evidence = record_readonly_compound_completion(
        &mut state,
        &request,
        "pwd && df -h",
        &output,
        std::path::Path::new("/tmp"),
        "completed",
        5,
    );
    assert_eq!(evidence.redaction_status, "excerpt_redacted");
}

#[test]
fn auto_mode_executes_when_request_cwd_is_unresolved() {
    // Regression from the real-provider acceptance rerun: cosh-core
    // control-permission requests can carry the "<unknown>" cwd
    // placeholder (no tracked command block). With zero command
    // activity, the shell's own prompt-time cwd report (the precmd
    // marker at the first prompt carries `$PWD`) says where the
    // shell sits, so the grant executes there — never in a guessed
    // process directory.
    let reported = std::env::temp_dir();
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        shell_prompt_cwd: Some(reported.display().to_string()),
        ..InlineState::default()
    };
    let mut output = Vec::new();

    let handled = render_auto_approved_tool(
        &mut state,
        &[compound_tool_call_event("pwd && df -h")],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert!(handled);
    let evidence = state
        .evidence
        .latest_shell_command_completed()
        .expect("executor completion evidence");
    assert_eq!(evidence.status, "completed");
    assert_eq!(evidence.exit_code, 0, "fallback cwd must execute");
    assert_eq!(
        evidence.cwd,
        reported.display().to_string(),
        "execution must use the shell-reported prompt cwd"
    );
}

#[test]
fn auto_mode_keeps_manual_flow_without_a_shell_cwd_report() {
    // A `cd` that produced no marker at all leaves the session with
    // zero observed activity AND no shell-side cwd report — the
    // absence of markers proves nothing about where the shell sits,
    // so an unknown request cwd must stay on the manual flow instead
    // of executing in a guessed directory.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    let mut output = Vec::new();

    render_auto_approved_tool(
        &mut state,
        &[compound_tool_call_event("pwd && df -h")],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert!(
        state.evidence.latest_shell_command_completed().is_none(),
        "a session without any shell cwd report must never auto-execute"
    );
}

#[test]
fn auto_mode_keeps_manual_flow_when_pty_input_follows_the_cwd_report() {
    // Freshness sequence: the initial prompt reported a directory,
    // then generic PTY input arrived — possibly a `cd` submitted
    // through a custom `accept-line` binding the byte-stream
    // heuristic cannot see — and its markers were lost entirely (no
    // CommandStarted/Completed/Failed and no fresh ShellReady). The
    // pre-input report no longer proves where the shell sits, so an
    // unknown request cwd must stay on the manual flow instead of
    // executing in the stale directory. Runs the real dispatch over
    // the cumulative event stream, then the bridge.
    let reported = std::env::temp_dir();
    let mut ready = ShellEvent::user_input_intercepted("session-1", "");
    ready.kind = ShellEventKind::ShellReady;
    ready.input = None;
    ready.cwd = Some(reported.display().to_string());
    let mut wrote = ShellEvent::user_input_intercepted("session-1", "");
    wrote.input = None;
    wrote.component = Some("shell_pty_input".to_string());
    wrote.message = Some("write".to_string());

    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    let mut output = Vec::new();
    crate::runtime::controller::render_inline_guidance(
        &[ready, wrote],
        &adapter,
        "bash",
        &mut state,
        &mut output,
    )
    .expect("dispatch cumulative events");
    assert_eq!(state.shell_prompt_cwd, None);

    render_auto_approved_tool(
        &mut state,
        &[compound_tool_call_event("pwd && df -h")],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert!(
        state.evidence.latest_shell_command_completed().is_none(),
        "a report invalidated by PTY input must never auto-execute"
    );
}

#[test]
fn auto_mode_keeps_manual_flow_when_the_shell_cwd_report_is_stale() {
    // The shell-reported prompt cwd is still validated: a report
    // naming a directory that no longer exists must not execute.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        shell_prompt_cwd: Some("/cosh-1882-stale-prompt-cwd".to_string()),
        ..InlineState::default()
    };
    let mut output = Vec::new();

    render_auto_approved_tool(
        &mut state,
        &[compound_tool_call_event("pwd && df -h")],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert!(
        state.evidence.latest_shell_command_completed().is_none(),
        "a stale shell cwd report must never auto-execute"
    );
}

/// A marker-tracked command block whose `end_cwd` is the shell's live
/// working directory.
fn session_block_with_end_cwd(end_cwd: &str) -> CommandBlock {
    CommandBlock {
        id: "cmd-live".to_string(),
        session_id: "session-1".to_string(),
        command: "cd somewhere".to_string(),
        origin: Default::default(),
        cwd: "/".to_string(),
        end_cwd: end_cwd.to_string(),
        started_at_ms: 0,
        ended_at_ms: 0,
        duration_ms: 0,
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

#[test]
fn auto_mode_prefers_the_live_shell_cwd_over_the_process_cwd() {
    // R8: after the user cd'd away, the latest tracked command block
    // carries the shell's live cwd — the executor must run there, not
    // in the cosh process launch directory.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    state
        .session_blocks
        .push(session_block_with_end_cwd("/tmp"));
    let mut output = Vec::new();

    let handled = render_auto_approved_tool(
        &mut state,
        &[compound_tool_call_event("pwd && df -h")],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert!(handled);
    let evidence = state
        .evidence
        .latest_shell_command_completed()
        .expect("executor completion evidence");
    assert_eq!(evidence.cwd, "/tmp");
    assert_eq!(evidence.exit_code, 0);
}

#[test]
fn auto_mode_keeps_manual_flow_when_the_live_cwd_is_unverifiable() {
    // R8: a tracked shell cwd that no longer exists must not be
    // silently replaced with a guess — the request stays on the
    // manual AskUser flow and nothing executes.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    state
        .session_blocks
        .push(session_block_with_end_cwd("/cosh-1882-vanished-dir"));
    let mut output = Vec::new();

    render_auto_approved_tool(
        &mut state,
        &[compound_tool_call_event("pwd && df -h")],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert!(
        state.evidence.latest_shell_command_completed().is_none(),
        "unverifiable cwd must never auto-execute"
    );
}

#[test]
fn auto_mode_keeps_manual_flow_when_the_explicit_cwd_is_stale() {
    // R9: an explicitly provided request cwd that no longer exists is
    // never silently replaced by any fallback — the request stays on
    // the manual AskUser flow.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    let mut run_request = compound_run_request();
    run_request.command_block.cwd = "/cosh-1882-stale-explicit-cwd".to_string();
    let mut output = Vec::new();

    render_auto_approved_tool(
        &mut state,
        &[compound_tool_call_event("pwd && df -h")],
        Some(&run_request),
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert!(
        state.evidence.latest_shell_command_completed().is_none(),
        "stale explicit cwd must never auto-execute"
    );
}

#[test]
fn auto_mode_keeps_manual_flow_when_markers_were_incomplete() {
    // Command activity that produced ledger errors instead of
    // blocks (incomplete/unmatched markers) is not proof the shell
    // never cd'd — the prompt-cwd fallback needs positive evidence of
    // zero activity, so this session stays on the manual flow even
    // when an earlier prompt report is available.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        shell_command_activity_observed: true,
        shell_prompt_cwd: Some(std::env::temp_dir().display().to_string()),
        ..InlineState::default()
    };
    let mut output = Vec::new();

    render_auto_approved_tool(
        &mut state,
        &[compound_tool_call_event("pwd && df -h")],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert!(
        state.evidence.latest_shell_command_completed().is_none(),
        "unproven session cwd must never fall back to the process cwd"
    );
}

#[test]
fn auto_mode_executor_accepts_expanded_readonly_forms() {
    // Issue #1882 executor widenings: a quoted separator is literal
    // argv rather than a connector, and a history-style token
    // stays literal argv, so both run through the executor. (The
    // newline-separated form is covered at the assessment and
    // executor levels; this ToolCall harness cannot carry a raw
    // newline inside a JSON string.)
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    for command in ["echo 'a && b' ; pwd", "echo !-2 && pwd"] {
        let mut state = InlineState {
            approval_mode: CoshApprovalMode::Auto,
            ..InlineState::default()
        };
        let run_request = compound_run_request();
        let mut output = Vec::new();

        let handled = render_auto_approved_tool(
            &mut state,
            &[compound_tool_call_event(command)],
            Some(&run_request),
            AgentRunOrigin::Standard,
            &mut output,
            &adapter,
        )
        .expect("render auto approval");

        assert!(handled, "{command}");
        assert!(
            state.control.shell_handoff().approved_is_empty(),
            "{command}"
        );
        let evidence = state
            .evidence
            .latest_shell_command_completed()
            .expect("executor completion evidence");
        assert_eq!(evidence.command, command);
        assert_eq!(evidence.exit_code, 0, "{command}");
        assert!(!evidence.provider_result_delivered, "{command}");
    }
}

#[test]
fn auto_mode_keeps_ineligible_compound_on_the_manual_path() {
    // Issue #1882 counter-cases: a segment without direct-readonly
    // evidence, an expansion token in argv, and a null-redirected
    // compound all fail plan building, so nothing is auto-approved,
    // executed, or recorded.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    for command in [
        "cd /tmp && git status",
        "echo $(pwd) && df -h",
        "pwd && df -h >/dev/null",
        "pwd\u{000c}&&\u{000c}df -h",
    ] {
        let mut state = InlineState {
            approval_mode: CoshApprovalMode::Auto,
            ..InlineState::default()
        };
        let mut output = Vec::new();

        render_auto_approved_tool(
            &mut state,
            &[compound_tool_call_event(command)],
            None,
            AgentRunOrigin::Standard,
            &mut output,
            &adapter,
        )
        .expect("render auto approval");

        assert!(
            state.control.shell_handoff().approved_is_empty(),
            "{command}"
        );
        assert!(
            state.evidence.latest_shell_command_completed().is_none(),
            "{command}"
        );
        assert!(state.approvals.requests.is_empty(), "{command}");
    }
}

#[test]
fn analysis_only_continuation_blocks_streamed_shell_tool_fallback() {
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    let governed = GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: None,
            name: "run_shell_command".to_string(),
            input: r#"{"command":"df -h"}"#.to_string(),
        },
        reason: "visible streamed tool call".to_string(),
        display_text: "visible streamed tool call".to_string(),
        auto_execute: false,
    };
    let mut output = Vec::new();

    let handled = render_auto_approved_tool(
        &mut state,
        &[governed],
        Some(&analysis_only_request()),
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    // #2639: refusing one event no longer claims the batch — that is what
    // used to strand the control approvals queued behind it. The refusal is
    // recorded as resolved instead, so nothing executes and no card asks the
    // user to approve a command the policy already turned down.
    assert!(!handled);
    assert_eq!(state.approvals.requests.len(), 1);
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Denied
    );
    assert_eq!(
        state.approvals.requests[0].execution_path,
        Some("not_executed_shell_request_policy_local")
    );
    assert!(state.control.shell_handoff().approved_is_empty());
}

#[test]
fn trust_mode_routes_blocked_shell_request_batch_to_approval() {
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    let governed = ["run-1", "run-2"].map(|run_id| GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolCall {
            run_id: run_id.to_string(),
            tool_id: None,
            name: "Bash".to_string(),
            input: "printf blocked\0binding".to_string(),
        },
        reason: "blocked shell binding".to_string(),
        display_text: "blocked shell binding".to_string(),
        auto_execute: false,
    });
    let mut output = Vec::new();

    crate::agent::events::render_agent_structured_events(
        &mut state,
        &governed,
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted approval");

    assert_eq!(state.approvals.requests.len(), 2);
    assert!(state.approvals.requests.iter().all(|request| {
        request.status == ApprovalRequestStatus::Pending
            && request
                .assessment
                .as_ref()
                .is_some_and(|assessment| assessment.execution == "block")
    }));
    assert_eq!(state.approvals.active_panel_id.as_deref(), Some("req-1"));
    assert!(state.approvals.active_panel_height > 0);
}

#[test]
fn trust_mode_keeps_unknown_provider_tool_pending() {
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    let governed = [GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolPermissionRequest {
            run_id: "run-1".to_string(),
            request_id: "ctrl-unknown".to_string(),
            tool_name: "CustomProviderTool".to_string(),
            tool_input: serde_json::json!({"operation": "mutate"}),
            tool_use_id: "toolu-unknown".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        },
        reason: "unknown provider tool".to_string(),
        display_text: "unknown provider tool".to_string(),
        auto_execute: false,
    }];
    let mut output = Vec::new();

    crate::agent::events::render_agent_structured_events(
        &mut state,
        &governed,
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render unknown provider approval");

    assert_eq!(state.approvals.requests.len(), 1);
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Pending
    );
    assert_eq!(
        state.approvals.requests[0].request_id.as_deref(),
        Some("ctrl-unknown")
    );
    let rendered = String::from_utf8(output).expect("approval output");
    assert!(
        rendered.contains("Outside the trusted tool catalog"),
        "{rendered}"
    );
}

#[test]
fn trust_mode_leaves_high_risk_shell_request_pending() {
    // #2064: Trust mode auto-approves ordinary commands, but an
    // irrecoverable one must still raise a per-dispatch approval card
    // and execute nothing.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    let governed = [GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: None,
            name: "Bash".to_string(),
            input: r#"{"command":"reboot"}"#.to_string(),
        },
        reason: "irrecoverable command".to_string(),
        display_text: "irrecoverable command".to_string(),
        auto_execute: false,
    }];
    let mut output = Vec::new();

    crate::agent::events::render_agent_structured_events(
        &mut state,
        &governed,
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted approval");

    assert_eq!(state.approvals.requests.len(), 1);
    let request = &state.approvals.requests[0];
    assert_eq!(request.status, ApprovalRequestStatus::Pending);
    assert_eq!(request.risk, "high");
    assert!(state.approvals.active_panel_id.is_some());
    assert!(state.control.shell_handoff().approved_is_empty());

    // A repeated dispatch of the same irrecoverable command must prompt
    // again: the card stays pending, no trust key is minted, and nothing
    // is queued for execution (#2064).
    crate::agent::events::render_agent_structured_events(
        &mut state,
        &governed,
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render repeated trusted approval");
    assert_eq!(state.approvals.requests.len(), 1);
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Pending
    );
    assert!(state.control.shell_handoff().approved_is_empty());

    // High verdicts outside the irrecoverable class keep Trust mode's
    // auto-approval contract (the raw-CLI evidence scenario runs
    // `i=$((i+1))` under Trust mode; stalling it broke the flow).
    let shell_syntax = [GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: None,
            name: "Bash".to_string(),
            input: r#"{"command":"i=$((i+1))"}"#.to_string(),
        },
        reason: "shell syntax".to_string(),
        display_text: "shell syntax".to_string(),
        auto_execute: false,
    }];
    crate::agent::events::render_agent_structured_events(
        &mut state,
        &shell_syntax,
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto-approved shell syntax");
    assert_eq!(state.approvals.requests.len(), 2);
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Pending
    );
    assert_eq!(
        state.approvals.requests[1].status,
        ApprovalRequestStatus::Approved
    );
}

#[test]
fn auto_mode_preloaded_trust_key_does_not_auto_approve_high_risk() {
    // #2064: `trusted_commands` can preload a session trust key for an
    // irrecoverable command; the high-risk gate must still win over
    // the trust-key auto-approval branch.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Auto,
        ..InlineState::default()
    };
    state.control.trust.trust_session_command(
        crate::approval::handoff::trust_key_from_command("reboot").expect("trust key"),
    );
    let governed = [GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: None,
            name: "Bash".to_string(),
            input: r#"{"command":"reboot"}"#.to_string(),
        },
        reason: "irrecoverable command".to_string(),
        display_text: "irrecoverable command".to_string(),
        auto_execute: false,
    }];
    let mut output = Vec::new();

    crate::agent::events::render_agent_structured_events(
        &mut state,
        &governed,
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render auto approval");

    assert_eq!(state.approvals.requests.len(), 1);
    let request = &state.approvals.requests[0];
    assert_eq!(request.status, ApprovalRequestStatus::Pending);
    assert_eq!(request.risk, "high");
    assert!(state.approvals.active_panel_id.is_some());
    assert!(state.control.shell_handoff().approved_is_empty());
}

#[test]
fn trust_mode_surfaces_hook_followup_approval_after_auto_approved_tool() {
    // #1920 regression: after the trust path auto-approves the shell
    // tool call, the sandbox-bypass follow-up approval reuses the same
    // tool_use_id via the control protocol with hook_requires_approval
    // set; it must surface as a pending card instead of being dropped.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    let tool_call = GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-1".to_string()),
            name: "Bash".to_string(),
            input: r#"{"command":"echo ok"}"#.to_string(),
        },
        reason: "provider tool call".to_string(),
        display_text: "provider tool call".to_string(),
        auto_execute: false,
    };
    let mut output = Vec::new();
    crate::agent::events::render_agent_structured_events(
        &mut state,
        &[tool_call],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted tool call");
    assert_eq!(state.approvals.requests.len(), 1);
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Approved
    );

    let followup = GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolPermissionRequest {
            run_id: "run-1".to_string(),
            request_id: "ctrl-2".to_string(),
            tool_name: "Bash".to_string(),
            tool_input: serde_json::json!({ "command": "echo ok" }),
            tool_use_id: "toolu-1".to_string(),
            hook_requires_approval: true,
            audit_ref: None,
        },
        reason: "sandbox bypass approval".to_string(),
        display_text: "sandbox bypass approval".to_string(),
        auto_execute: false,
    };
    crate::agent::events::render_agent_structured_events(
        &mut state,
        &[followup],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render hook follow-up approval");

    let pending = state
        .approvals
        .requests
        .iter()
        .find(|request| request.request_id.as_deref() == Some("ctrl-2"))
        .expect("hook follow-up approval must be recorded");
    assert_eq!(pending.status, ApprovalRequestStatus::Pending);
    assert!(pending.hook_requires_approval);
    assert_eq!(pending.tool_use_id.as_deref(), Some("toolu-1"));
    assert_eq!(
        state.approvals.active_panel_id.as_deref(),
        Some(pending.id.as_str())
    );
}

#[test]
fn shell_request_policy_denies_duplicate_host_executed_request() {
    let mut state = InlineState::default();
    state
        .control
        .provider_tool_mut()
        .claim_host_executed_shell_result("run-1", "ctrl-1", Some("toolu-1"))
        .expect("claim host result");
    let request = shell_request(
        ProviderShellRequestKind::ControlPermission,
        Some("ctrl-1"),
        Some("toolu-1"),
    );

    assert_eq!(
        shell_request_policy_decision(&state, None, &request),
        ShellRequestPolicyDecision::DenyDuplicateHostExecuted
    );

    let mut next_run_request = request;
    next_run_request.run_id = "run-2".to_string();
    assert_eq!(
        shell_request_policy_decision(&state, None, &next_run_request),
        ShellRequestPolicyDecision::Continue
    );
}

#[test]
fn shell_request_policy_denies_reentrant_fallback_after_handoff() {
    let mut state = InlineState::default();
    state.control.mark_provider_shell_handoff_run("run-1");
    let request = shell_request(
        ProviderShellRequestKind::StreamedToolCallFallback,
        None,
        None,
    );

    assert_eq!(
        shell_request_policy_decision(&state, None, &request),
        ShellRequestPolicyDecision::DenyAnalysisOnly
    );
}

#[test]
fn shell_request_policy_allows_new_control_shell_request() {
    let state = InlineState::default();
    let request = shell_request(
        ProviderShellRequestKind::ControlPermission,
        Some("ctrl-2"),
        Some("toolu-2"),
    );

    assert_eq!(
        shell_request_policy_decision(&state, None, &request),
        ShellRequestPolicyDecision::Continue
    );
}

fn shell_request(
    provider_shell_request_kind: ProviderShellRequestKind,
    request_id: Option<&str>,
    tool_use_id: Option<&str>,
) -> RuntimeApprovalRequest {
    RuntimeApprovalRequest {
        id: "req-1".to_string(),
        audit_ref: None,
        run_id: "run-1".to_string(),
        origin: AgentRunOrigin::Standard,
        session_id: "sess-1".to_string(),
        cwd: "/tmp".to_string(),
        source: "test",
        provider_shell_request_kind,
        kind: ApprovalRequestKind::Tool,
        subject: "run_shell_command".to_string(),
        preview: "$ df -h".to_string(),
        risk: "medium",
        request_id: request_id.map(str::to_string),
        tool_use_id: tool_use_id.map(str::to_string),
        tool_input: Some(serde_json::json!({ "command": "df -h" })),
        original_user_request: None,
        status: ApprovalRequestStatus::Approved,
        execution_path: None,
        command_block_id: None,
        redaction_status: None,
        assessment: None,
        hook_requires_approval: false,
        hook_warnings: Vec::new(),
    }
}

fn replayed_shell_request(tool_use_id: Option<&str>) -> RuntimeApprovalRequest {
    RuntimeApprovalRequest {
        id: "req-replay".to_string(),
        audit_ref: None,
        run_id: "run-1".to_string(),
        origin: AgentRunOrigin::Standard,
        session_id: "sess-1".to_string(),
        cwd: "/tmp".to_string(),
        source: "provider-tool-call",
        provider_shell_request_kind: ProviderShellRequestKind::StreamedToolCallFallback,
        kind: ApprovalRequestKind::Tool,
        subject: "run_shell_command".to_string(),
        preview: "$ reboot".to_string(),
        risk: "high",
        request_id: None,
        tool_use_id: tool_use_id.map(str::to_string),
        tool_input: None,
        original_user_request: None,
        status: ApprovalRequestStatus::Pending,
        execution_path: None,
        command_block_id: None,
        redaction_status: None,
        assessment: None,
        hook_requires_approval: false,
        hook_warnings: Vec::new(),
    }
}

fn journal_entry_for(
    request: &RuntimeApprovalRequest,
    actor: &'static str,
) -> RuntimeApprovalJournalEntry {
    RuntimeApprovalJournalEntry {
        id: "req-1".to_string(),
        audit_ref: None,
        run_id: request.run_id.clone(),
        source: request.source,
        kind: request.kind,
        subject: request.subject.clone(),
        preview: request.preview.clone(),
        preview_hash: String::new(),
        risk: request.risk,
        request_id: None,
        tool_use_id: request.tool_use_id.clone(),
        actor,
        decision: ApprovalRequestStatus::Approved,
        execution_path: None,
        command_block_id: None,
        redaction_status: None,
        assessment: None,
    }
}

/// Replay receipts echo the actual resolution (#2064): a manual Allow
/// reads Approved, turn consent reads the turn title, and without a
/// matching journal record the title falls back to Auto-approved.
#[test]
fn replayed_shell_receipt_title_reflects_actual_resolution() {
    let request = replayed_shell_request(Some("toolu-1"));

    let mut state = InlineState::default();
    state
        .approvals
        .journal
        .push(journal_entry_for(&request, "user"));
    assert_eq!(
        completed_provider_native_shell_title(&state, &request),
        MessageId::ApprovalResolutionApprovedTitle
    );

    let mut state = InlineState::default();
    state
        .approvals
        .journal
        .push(journal_entry_for(&request, "batch_consent"));
    assert_eq!(
        completed_provider_native_shell_title(&state, &request),
        MessageId::ApprovalResolutionTurnApprovedTitle
    );

    // agent-auto resolution and empty journal both stay Auto-approved.
    let mut state = InlineState::default();
    state
        .approvals
        .journal
        .push(journal_entry_for(&request, "agent-auto"));
    assert_eq!(
        completed_provider_native_shell_title(&state, &request),
        MessageId::ApprovalResolutionAutoApprovedTitle
    );
    assert_eq!(
        completed_provider_native_shell_title(&InlineState::default(), &request),
        MessageId::ApprovalResolutionAutoApprovedTitle
    );

    // Without tool ids the preview anchors the match.
    let request = replayed_shell_request(None);
    let mut state = InlineState::default();
    state
        .approvals
        .journal
        .push(journal_entry_for(&request, "user"));
    assert_eq!(
        completed_provider_native_shell_title(&state, &request),
        MessageId::ApprovalResolutionApprovedTitle
    );
}

#[test]
fn carried_system_control_defeats_trust_key_and_trust_mode() {
    // #2064 rounds 6-7: a payload hiding a system-control program —
    // carried (`sh -c 'sudo reboot'`), a whole-machine systemctl verb,
    // or an opaque carried construct that fails closed as Unresolved
    // (`sh -c 'echo $(reboot)'`) — must hit the gate on every dispatch.
    // A session trust key minted for the exact form, and Trust mode,
    // both leave a pending card, and a repeat dispatch prompts again
    // instead of running silently through the key.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    for command in [
        "sh -c 'sudo reboot'",
        "systemctl reboot",
        "sh -c 'echo $(reboot)'",
    ] {
        for (approval_mode, label) in [
            (CoshApprovalMode::Auto, "auto"),
            (CoshApprovalMode::Trust, "trust"),
        ] {
            let mut state = InlineState {
                approval_mode,
                ..InlineState::default()
            };
            state.control.trust.trust_session_command(
                crate::approval::handoff::trust_key_from_command(command).expect("trust key"),
            );
            let governed = [GovernedEvent {
                decision: GovernanceDecision::Display,
                policy_decision: GovernancePolicyDecision::NeedsUserApproval,
                event: AgentEvent::ToolCall {
                    run_id: "run-1".to_string(),
                    tool_id: None,
                    name: "Bash".to_string(),
                    input: format!(r#"{{"command":"{command}"}}"#),
                },
                reason: "irrecoverable command".to_string(),
                display_text: "irrecoverable command".to_string(),
                auto_execute: false,
            }];
            for dispatch in 0..2 {
                let mut output = Vec::new();
                crate::agent::events::render_agent_structured_events(
                    &mut state,
                    &governed,
                    None,
                    AgentRunOrigin::Standard,
                    &mut output,
                    &adapter,
                )
                .expect("render carried system-control dispatch");
                assert_eq!(
                    state.approvals.requests.len(),
                    1,
                    "{command}: {label} dispatch {dispatch}"
                );
                assert_eq!(
                    state.approvals.requests[0].status,
                    ApprovalRequestStatus::Pending,
                    "{command}: {label} dispatch {dispatch}"
                );
                assert_eq!(state.approvals.requests[0].risk, "high");
                assert!(
                    state.control.shell_handoff().approved_is_empty(),
                    "{command}: {label} dispatch {dispatch}"
                );
            }
        }
    }
}

fn streamed_bash_tool_call() -> GovernedEvent {
    GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        event: AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("toolu-staged".to_string()),
            name: "Bash".to_string(),
            input: r#"{"command":"echo staged"}"#.to_string(),
        },
        reason: "grace-released staged tool call".to_string(),
        display_text: "grace-released staged tool call".to_string(),
        auto_execute: false,
    }
}

fn active_run_with_provider(provider_name: &'static str) -> crate::agent::run::ActiveAgentRun {
    let (mut active_run, _approval_rx) =
        crate::agent::run::test_support::test_active_run_with_id("run-1");
    active_run.provider_name = provider_name;
    active_run
}

#[test]
fn cosh_core_grace_released_tool_call_with_block_verdict_journals_rejection() {
    // #2156: the hook verdict arrives inside the staging window as a block
    // and the core releases the normalized provider-native error result
    // ("Blocked by hook: ..."); the staged call must be journaled as a
    // rejection, never replayed as auto-approved and executed.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    state.agent_run.active = Some(active_run_with_provider("cosh-core"));
    // The activity path already surfaced the core's block verdict marker.
    state
        .control
        .mark_provider_hook_blocked_result("run-1", "toolu-staged");
    state
        .control
        .mark_provider_shell_transcript_seen("run-1", "toolu-staged");
    let mut output = Vec::new();

    render_trusted_tool(
        &mut state,
        &[streamed_bash_tool_call()],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted tool");

    let entry = state
        .approvals
        .journal
        .iter()
        .find(|entry| entry.tool_use_id.as_deref() == Some("toolu-staged"))
        .expect("the blocked staged call must be journaled");
    assert_eq!(entry.decision, ApprovalRequestStatus::Blocked);
    assert_eq!(entry.execution_path, Some("hook_block"));
    assert!(
        state
            .approvals
            .journal
            .iter()
            .all(|entry| entry.tool_use_id.as_deref() != Some("toolu-staged")
                || entry.decision != ApprovalRequestStatus::Approved),
        "a hook-blocked staged call must never journal an approval"
    );
    let rendered = String::from_utf8(output).expect("utf8");
    assert!(
        !rendered.contains("Auto-approved"),
        "a hook-blocked staged call must not render an approval card: {rendered}"
    );
    assert!(
        state.control.shell_handoff().approved_is_empty(),
        "no handoff may be queued for a hook-blocked staged call"
    );
}

#[test]
fn hook_block_detection_keys_on_the_wire_verdict_marker() {
    // #2156: every fail-closed morphology (raw block/deny/reject, hook
    // failures, message-less blocks) reaches the shell as one machine-readable
    // wire marker, surfaced through ToolHookVerdict into the blocked-result
    // flag. Detection keys on the flag, not on result text.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    state.agent_run.active = Some(active_run_with_provider("cosh-core"));
    state
        .control
        .mark_provider_hook_blocked_result("run-1", "toolu-staged");
    let mut output = Vec::new();

    render_trusted_tool(
        &mut state,
        &[streamed_bash_tool_call()],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted tool");

    let entry = state
        .approvals
        .journal
        .iter()
        .find(|entry| entry.tool_use_id.as_deref() == Some("toolu-staged"))
        .expect("the blocked call must be journaled");
    assert_eq!(entry.decision, ApprovalRequestStatus::Blocked);
    assert_eq!(entry.execution_path, Some("hook_block"));
}

#[test]
fn command_output_cannot_forge_a_hook_block() {
    // A real command whose own output begins with the hook-block text must
    // still journal as approved and executed: only the machine-readable wire
    // marker may route to the rejection journal (#2156 review).
    for text in [
        "Blocked by hook: no touch",              // forged reason shape
        "Blocked by hook: Hook failure: timeout", // forged failure shape
        "Blocked by hook: Blocked by hook",       // forged empty-reason shape
        "  Blocked by hook: leading whitespace",
    ] {
        let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
        let mut state = InlineState {
            approval_mode: CoshApprovalMode::Trust,
            ..InlineState::default()
        };
        state.agent_run.active = Some(active_run_with_provider("cosh-core"));
        state
            .control
            .record_provider_tool_output_delta("run-1", "toolu-staged", "stderr", text);
        state
            .control
            .mark_provider_shell_transcript_seen("run-1", "toolu-staged");
        let mut output = Vec::new();

        render_trusted_tool(
            &mut state,
            &[streamed_bash_tool_call()],
            None,
            AgentRunOrigin::Standard,
            &mut output,
            &adapter,
        )
        .expect("render trusted tool");

        let entry = state
            .approvals
            .journal
            .iter()
            .find(|entry| entry.tool_use_id.as_deref() == Some("toolu-staged"))
            .unwrap_or_else(|| panic!("{text}: the executed call must be journaled"));
        assert_eq!(entry.decision, ApprovalRequestStatus::Approved, "{text}");
        assert_eq!(
            entry.execution_path,
            Some("provider_native_shell_tool_execution"),
            "{text}"
        );
    }
}

#[test]
fn late_allow_verdict_reconciles_the_provisional_staged_entry() {
    // #2156: the hook takes longer than the 200 ms grace, so M3 journals the
    // provisional staged_unresolved first; the late can_use_tool verdict then
    // approves and executes. The journal must end with exactly one terminal
    // entry that matches what actually happened — not a contradictory
    // Blocked+Approved pair.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    state.agent_run.active = Some(active_run_with_provider("cosh-core"));
    let mut output = Vec::new();

    render_trusted_tool(
        &mut state,
        &[streamed_bash_tool_call()],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted tool");
    let provisional = state
        .approvals
        .journal
        .iter()
        .find(|entry| entry.tool_use_id.as_deref() == Some("toolu-staged"))
        .expect("M3 must journal the desync");
    assert_eq!(provisional.execution_path, Some("staged_unresolved"));

    // The late control-channel verdict arrives and trust auto-approves it.
    let late_request = shell_request(
        ProviderShellRequestKind::ControlPermission,
        Some("ctrl-late"),
        Some("toolu-staged"),
    );
    crate::approval::requests::record_auto_approved_request(&mut state, late_request);

    let entries = state
        .approvals
        .journal
        .iter()
        .filter(|entry| entry.tool_use_id.as_deref() == Some("toolu-staged"))
        .count();
    assert_eq!(
        entries, 1,
        "each tool_use_id must have exactly one terminal journal entry"
    );
    let entry = state
        .approvals
        .journal
        .iter()
        .find(|entry| entry.tool_use_id.as_deref() == Some("toolu-staged"))
        .expect("the reconciled entry");
    assert_eq!(entry.decision, ApprovalRequestStatus::Approved);
    assert_eq!(
        entry.execution_path,
        Some("staged_resolved_late_verdict"),
        "the reconciled entry keeps the late-verdict provenance"
    );
}

#[test]
fn executed_but_failed_result_still_journals_the_approval() {
    // A genuinely executed command that failed (nonzero exit) keeps the
    // completed-replay semantics: it was approved and did run. Only the
    // core's normalized block marker routes to the rejection journal.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    state.agent_run.active = Some(active_run_with_provider("cosh-core"));
    state.control.record_provider_tool_output_delta(
        "run-1",
        "toolu-staged",
        "stderr",
        "permission denied",
    );
    state
        .control
        .mark_provider_shell_transcript_seen("run-1", "toolu-staged");
    let mut output = Vec::new();

    render_trusted_tool(
        &mut state,
        &[streamed_bash_tool_call()],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted tool");

    let entry = state
        .approvals
        .journal
        .iter()
        .find(|entry| entry.tool_use_id.as_deref() == Some("toolu-staged"))
        .expect("the executed call must be journaled");
    assert_eq!(entry.decision, ApprovalRequestStatus::Approved);
    assert_eq!(
        entry.execution_path,
        Some("provider_native_shell_tool_execution")
    );
}

#[test]
fn late_verdict_reconcile_is_scoped_to_its_own_run() {
    // #2156: two consecutive runs can reuse the same tool id; a late verdict
    // from the newer run must never rewrite the older run's provisional
    // entry, and each run keeps its own terminal state.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    state.agent_run.active = Some(active_run_with_provider("cosh-core"));
    let mut output = Vec::new();

    // Run 1 leaves a provisional staged_unresolved entry for the shared id.
    render_trusted_tool(
        &mut state,
        &[streamed_bash_tool_call()],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted tool");

    // A new run starts; its late verdict arrives for the same tool id.
    if let Some(active) = state.agent_run.active.as_mut() {
        active.request.id = "run-2".to_string();
    }
    let late_request = RuntimeApprovalRequest {
        run_id: "run-2".to_string(),
        ..shell_request(
            ProviderShellRequestKind::ControlPermission,
            Some("ctrl-late"),
            Some("toolu-staged"),
        )
    };
    crate::approval::requests::record_auto_approved_request(&mut state, late_request);

    let run1 = state
        .approvals
        .journal
        .iter()
        .find(|entry| {
            entry.run_id == "run-1" && entry.tool_use_id.as_deref() == Some("toolu-staged")
        })
        .expect("run-1 provisional entry must remain");
    assert_eq!(run1.decision, ApprovalRequestStatus::Blocked);
    assert_eq!(
        run1.execution_path,
        Some("staged_unresolved"),
        "the older run's provisional entry must not be rewritten by another run"
    );
    let run2 = state
        .approvals
        .journal
        .iter()
        .find(|entry| {
            entry.run_id == "run-2" && entry.tool_use_id.as_deref() == Some("toolu-staged")
        })
        .expect("run-2 must journal its own terminal approval");
    assert_eq!(run2.decision, ApprovalRequestStatus::Approved);
}

#[test]
fn cosh_core_grace_released_tool_call_never_executes() {
    // M3 (#2067): under cosh-core a bare grace-released ToolCall has no
    // core-visible verdict; it must be journaled as staged_unresolved
    // instead of auto-approved or handed off.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    state.agent_run.active = Some(active_run_with_provider("cosh-core"));
    let mut output = Vec::new();

    render_trusted_tool(
        &mut state,
        &[streamed_bash_tool_call()],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted tool");

    assert!(
        state.approvals.requests.is_empty(),
        "no approval request may be created for an unresolved staged call"
    );
    assert!(
        state.control.shell_handoff().approved_is_empty(),
        "no handoff may be queued for an unresolved staged call"
    );
    let entry = state
        .approvals
        .journal
        .iter()
        .find(|entry| entry.tool_use_id.as_deref() == Some("toolu-staged"))
        .expect("the desync must be journaled");
    assert_eq!(entry.execution_path, Some("staged_unresolved"));
}

#[test]
fn non_cosh_core_grace_released_tool_call_keeps_legacy_fallback() {
    // I4/R4: claude/qwen report control_protocol but have no core verdict
    // channel, so their grace-release fallback must keep auto-approving.
    let adapter = AdapterInstance::QwenCli(QwenCliAdapter::default());
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..InlineState::default()
    };
    state.agent_run.active = Some(active_run_with_provider("qwen"));
    let mut output = Vec::new();

    render_trusted_tool(
        &mut state,
        &[streamed_bash_tool_call()],
        None,
        AgentRunOrigin::Standard,
        &mut output,
        &adapter,
    )
    .expect("render trusted tool");

    assert_eq!(state.approvals.requests.len(), 1);
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Approved
    );
    assert!(
        state
            .approvals
            .journal
            .iter()
            .all(|entry| entry.execution_path != Some("staged_unresolved")),
        "the M3 guard must not reach non-cosh-core drivers"
    );
}
