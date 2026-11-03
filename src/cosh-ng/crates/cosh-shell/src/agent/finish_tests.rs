use std::time::Instant;

use super::*;
use crate::agent::events::render_active_agent_event;
use crate::agent::run::{PendingAgentRequest, PendingRequestClass};
use crate::runtime::state::InlineState;
use crate::types::{AgentMode, AgentRequest, CommandBlock, CommandStatus, OutputRefs};

fn request(id: &str) -> AgentRequest {
    AgentRequest {
        id: id.to_string(),
        session_id: "shell-session".to_string(),
        command_block: CommandBlock {
            id: format!("cmd-{id}"),
            session_id: "shell-session".to_string(),
            command: "echo hi".to_string(),
            origin: Default::default(),
            cwd: "/repo".to_string(),
            end_cwd: "/repo".to_string(),
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
        user_input: Some("queued".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

fn pending(id: &str, class: PendingRequestClass) -> PendingAgentRequest {
    PendingAgentRequest {
        request: request(id),
        origin: AgentRunOrigin::Standard,
        intent: AgentStartIntent::UserInitiated,
        class,
        selectable_after_event_index: None,
        before_held_text: false,
    }
}

// A provider that ends its turn while its own Bash handoff is still running
// produced that text without the command's result, so finishing the run must
// drop it instead of rendering it or moving it to the shell-wide hold. The
// shell-evidence continuation regenerates the answer.
#[test]
fn finish_drops_held_text_while_its_own_shell_handoff_is_still_outstanding() {
    let (rendered, state) = finish_run_with_handoff_owned_by("run-1");

    assert!(!rendered.contains("STALE ANSWER"), "{rendered}");
    assert!(state.agent_run.held_events.is_empty());
}

// The drop is scoped to the run that owns the handoff: text from a run whose
// own work is complete must still be shown even while some other run's
// handoff is in flight.
#[test]
fn finish_keeps_held_text_when_another_run_owns_the_outstanding_handoff() {
    let (rendered, _state) = finish_run_with_handoff_owned_by("unrelated-run");

    assert!(rendered.contains("STALE ANSWER"), "{rendered}");
}

#[test]
fn finish_renders_orphan_hook_notification_with_fallbacks() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut active_run = active_run(&adapter, "run-1", Language::ZhCn);
    active_run
        .pending_hook_notifications
        .push(PendingHookNotification {
            tool_use_id: Some("tool-1".to_string()),
            hook_name: "  ".to_string(),
            message: String::new(),
            decision: Some("\n".to_string()),
        });
    state.agent_run.active = Some(active_run);
    let mut output = Vec::new();

    finish_active_agent_run(&mut state, &mut output, &adapter).expect("finish run");

    let rendered = String::from_utf8_lossy(&output);
    assert!(rendered.contains("Hook: 未知 Hook"), "{rendered}");
    assert!(rendered.contains("消息: 未提供消息"), "{rendered}");
    assert!(rendered.contains("决策: 未指定"), "{rendered}");
    assert!(!rendered.contains("unknown hook"), "{rendered}");
}

// #2197: a hook that fires on every tool call used to dump one three-line
// block per hit into the same Governance panel. The finish path must render
// the collapsed projection while `deferred_events` keeps every original.
#[test]
fn finish_collapses_repeated_allow_hook_notifications_into_one_line() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut active_run = active_run(&adapter, "run-1", Language::EnUs);
    for _ in 0..3 {
        active_run
            .pending_hook_notifications
            .push(PendingHookNotification {
                tool_use_id: Some("tool-1".to_string()),
                hook_name: "pii-checker".to_string(),
                message: "[pii-checker] card hit".to_string(),
                decision: Some("allow".to_string()),
            });
    }
    state.agent_run.active = Some(active_run);
    let mut output = Vec::new();

    finish_active_agent_run(&mut state, &mut output, &adapter).expect("finish run");

    let rendered = String::from_utf8_lossy(&output);
    assert_eq!(rendered.matches("pii-checker").count(), 1, "{rendered}");
    assert!(
        rendered.contains("• pii-checker: card hit ×3"),
        "{rendered}"
    );
    assert!(!rendered.contains("Decision: allow"), "{rendered}");
}

#[test]
fn recovery_context_collapses_allow_notices_and_omits_provider_timeout() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut active_run = active_run(&adapter, "run-1", Language::EnUs);
    let mut ingress_output = Vec::new();
    for _ in 0..3 {
        render_active_agent_event(
            &mut active_run,
            AgentEvent::HookNotification {
                run_id: "run-1".to_string(),
                hook_name: "pii-checker".to_string(),
                message: "[pii-checker] card hit".to_string(),
                tool_use_id: Some("tool-1".to_string()),
                decision: Some("allow".to_string()),
            },
            &mut ingress_output,
            None,
        )
        .expect("ingest hook notification");
    }
    render_active_agent_event(
        &mut active_run,
        AgentEvent::AgentFailed {
            run_id: "run-1".to_string(),
            error: "PROVIDER TIMEOUT MUST STAY HIDDEN".to_string(),
            error_code: Some(PROVIDER_TIMEOUT_ERROR_CODE.to_string()),
            max_turns: None,
        },
        &mut ingress_output,
        None,
    )
    .expect("ingest provider timeout");
    assert_eq!(active_run.pending_hook_notifications.len(), 3);
    let mut output = Vec::new();

    render_recovery_deferred_context(&active_run, &mut output).expect("render recovery context");

    let rendered = String::from_utf8_lossy(&output);
    assert_eq!(rendered.matches("pii-checker").count(), 1, "{rendered}");
    assert!(
        rendered.contains("• pii-checker: card hit ×3"),
        "{rendered}"
    );
    assert!(
        !rendered.contains("PROVIDER TIMEOUT MUST STAY HIDDEN"),
        "{rendered}"
    );
}

// Finishes an active `run-1` holding one text delta, with an approved handoff
// owned by `handoff_run_id` still outstanding. Returns what was rendered plus
// the resulting state.
fn finish_run_with_handoff_owned_by(handoff_run_id: &str) -> (String, InlineState) {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut active_run = active_run(&adapter, "run-1", Language::EnUs);
    active_run.held_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::TextDelta {
            run_id: "run-1".to_string(),
            text: "STALE ANSWER".to_string(),
        },
        reason: "held".to_string(),
        display_text: "STALE ANSWER".to_string(),
        auto_execute: false,
    });
    state.agent_run.active = Some(active_run);
    state.control.shell_handoff_mut().enqueue_approved_request(
        ShellHandoffRequest::new(
            "df -h",
            "$ df -h",
            "provider-tool-call",
            "agent",
            "req-1",
            handoff_run_id,
            1,
        )
        .expect("handoff"),
    );
    let mut output = Vec::new();

    finish_active_agent_run(&mut state, &mut output, &adapter).expect("finish run");

    (String::from_utf8_lossy(&output).to_string(), state)
}

fn active_run(adapter: &AdapterInstance, id: &str, language: Language) -> ActiveAgentRun {
    let run_request = request(id);
    let handle = adapter.start_cancellable(run_request.clone(), CoshApprovalMode::Recommend);
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(language);
    ActiveAgentRun {
        request: run_request,
        origin: AgentRunOrigin::Standard,
        handle,
        provider_name: "fake",
        language,
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
        completed: true,
        host_completed_tool_ids: Vec::new(),
        pending_hook_notifications: Vec::new(),
    }
}

#[test]
fn provider_timeout_trim_keeps_control_responses_and_oldest_normal_in_order() {
    let mut state = InlineState::default();
    for (id, class) in [
        ("normal-a", PendingRequestClass::Normal),
        ("control-b", PendingRequestClass::ControlResponse),
        ("normal-c", PendingRequestClass::Normal),
        ("control-d", PendingRequestClass::ControlResponse),
        ("normal-e", PendingRequestClass::Normal),
    ] {
        state
            .agent_run
            .queued_requests
            .push_back(pending(id, class));
    }

    let dropped = trim_queued_requests_after_provider_timeout(&mut state);

    // Only the surplus normal requests were dropped and counted; every
    // control response survives, and FIFO order is untouched.
    assert_eq!(dropped, 2);
    let ids: Vec<&str> = state
        .agent_run
        .queued_requests
        .iter()
        .map(|pending| pending.request.id.as_str())
        .collect();
    assert_eq!(ids, ["normal-a", "control-b", "control-d"]);
}

#[test]
fn provider_timeout_trim_drops_nothing_without_surplus_normals() {
    let mut state = InlineState::default();
    state
        .agent_run
        .queued_requests
        .push_back(pending("control-a", PendingRequestClass::ControlResponse));
    state
        .agent_run
        .queued_requests
        .push_back(pending("normal-b", PendingRequestClass::Normal));

    assert_eq!(trim_queued_requests_after_provider_timeout(&mut state), 0);
    assert_eq!(state.agent_run.queued_requests.len(), 2);
}

// F2 (#2067): an orphan hook block notification must drain through
// governance so the deferred render carries real display text instead of
// an empty card.
#[test]
fn finish_drains_orphan_hook_notification_with_visible_display_text() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut active_run = active_run(&adapter, "run-1", Language::EnUs);
    active_run
        .pending_hook_notifications
        .push(crate::agent::run::PendingHookNotification {
            tool_use_id: Some("toolu-1".to_string()),
            hook_name: "guard".to_string(),
            message: String::new(),
            decision: Some("block".to_string()),
        });
    state.agent_run.active = Some(active_run);
    let mut output = Vec::new();

    finish_active_agent_run(&mut state, &mut output, &adapter).expect("finish run");

    let rendered = String::from_utf8(output).expect("utf8");
    assert!(
        rendered.contains("Hook: guard")
            && rendered.contains("Message: no message provided")
            && rendered.contains("Decision: block"),
        "the orphan block must render visible governance text: {rendered}"
    );
}
#[test]
fn terminal_run_revokes_pending_approval_capture_and_late_authority_only_for_owner() {
    for (failed, foreign_pending) in [(true, false), (true, true), (false, false)] {
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut state = InlineState::default();
        let mut run = active_run(&adapter, "run-1", Language::EnUs);
        let permission = GovernedEvent {
            policy_decision: GovernancePolicyDecision::NeedsUserApproval,
            decision: GovernanceDecision::Display,
            display_text: "approval required".to_string(),
            reason: "needs user approval".to_string(),
            auto_execute: false,
            event: AgentEvent::ToolPermissionRequest {
                run_id: "run-1".to_string(),
                request_id: "ctrl-eof".to_string(),
                tool_name: "shell".to_string(),
                tool_input: serde_json::json!({"command":"printf unapproved >> /tmp/owned-eof-effect"}),
                tool_use_id: "tool-eof".to_string(),
                hook_requires_approval: false,
                audit_ref: None,
            },
        };
        let ids = crate::approval::requests::record_approval_requests(
            &mut state,
            &[permission],
            Some(&run.request),
            AgentRunOrigin::Standard,
            false,
        );
        assert_eq!(ids.len(), 1);
        let id = ids[0].clone();
        let mut approved = state.approvals.requests[0].clone();
        approved.id = "already-approved".to_string();
        approved.status = ApprovalRequestStatus::Approved;
        state.approvals.requests.push(approved);
        if foreign_pending {
            let mut foreign = state.approvals.requests[0].clone();
            foreign.id = "foreign-pending".to_string();
            foreign.run_id = "other-run".to_string();
            state.approvals.requests.push(foreign);
        }
        state
            .control
            .approval_ledger_mut()
            .register("run-1", "ctrl-eof");
        state
            .control
            .trust
            .grant_run_batch_consent("run-1".to_string());
        state
            .approvals
            .focus
            .insert(id.clone(), ApprovalPanelAction::Approve);
        state.approvals.expanded_cards.insert(id.clone());
        let mut output = Vec::new();
        crate::approval::panel::render_current_approval_request(&mut state, &mut output).unwrap();
        assert!(crate::runtime::controller::pending_card_capture(&state).is_some());
        if failed {
            render_active_agent_event(
                &mut run,
                AgentEvent::AgentFailed {
                    run_id: "run-1".to_string(),
                    error: "cosh-core output reached EOF".to_string(),
                    error_code: None,
                    max_turns: None,
                },
                &mut output,
                None,
            )
            .unwrap();
        }
        state.agent_run.active = Some(run);
        finish_active_agent_run(&mut state, &mut output, &adapter).unwrap();
        if !failed {
            assert_eq!(
                state.approvals.requests[0].status,
                ApprovalRequestStatus::Pending
            );
            assert!(crate::runtime::controller::pending_card_capture(&state).is_some());
            let mut approve = ShellEvent::user_input_intercepted("shell-session", &id);
            approve.component = Some("card".to_string());
            approve.message = Some("approve".to_string());
            crate::approval::runtime::render_approval_actions(
                &[approve],
                &[],
                &adapter,
                &mut state,
                &mut output,
                98,
            )
            .unwrap();
            assert_eq!(
                state.approvals.requests[0].status,
                ApprovalRequestStatus::Approved
            );
            assert!(state.control.shell_handoff().has_active_handoff());
            continue;
        }
        assert_eq!(
            state.approvals.requests[0].status,
            ApprovalRequestStatus::Cancelled
        );
        assert_eq!(
            state.approvals.requests[1].status,
            ApprovalRequestStatus::Approved
        );
        assert!(!state.approvals.focus.contains_key(&id));
        assert!(!state.approvals.expanded_cards.contains(&id));
        assert!(state.control.trust.run_batch_consent().is_none());
        assert!(state
            .control
            .approval_ledger()
            .unresponded_for_run("run-1")
            .is_empty());
        assert_eq!(state.approvals.journal.len(), 1);
        assert_eq!(state.approvals.journal[0].actor, "run_terminal");
        if foreign_pending {
            assert_eq!(
                state.approvals.requests[2].status,
                ApprovalRequestStatus::Pending
            );
        } else {
            assert!(crate::runtime::controller::pending_card_capture(&state).is_none());
            assert!(state.agent_run.needs_prompt_after_run);
        }
        let mut late = ShellEvent::user_input_intercepted("shell-session", &id);
        late.component = Some("card".to_string());
        late.message = Some("approve_turn".to_string());
        crate::approval::runtime::render_approval_actions(
            &[late],
            &[],
            &adapter,
            &mut state,
            &mut output,
            99,
        )
        .unwrap();
        assert!(state.agent_run.active.is_none());
        assert!(state.agent_run.queued_requests.is_empty());
        assert!(!state.control.shell_handoff().has_active_handoff());
        assert!(state.control.trust.run_batch_consent().is_none());
        assert_eq!(state.approvals.journal.len(), 1);
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("cosh-core output reached EOF"));
    }
}
#[test]
fn failed_run_does_not_auto_execute_unrendered_approval_tail() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Trust,
        ..Default::default()
    };
    let mut run = active_run(&adapter, "run-tail", Language::EnUs);
    for event in [
        AgentEvent::ToolCall {
            run_id: "run-tail".to_string(),
            tool_id: Some("completed-tool".to_string()),
            name: "read_file".to_string(),
            input: r#"{"path":"/tmp/already-read"}"#.to_string(),
        },
        AgentEvent::ToolOutputDelta {
            run_id: "run-tail".to_string(),
            tool_id: "completed-tool".to_string(),
            stream: "stdout".to_string(),
            text: "COMPLETED_OUTPUT_BEFORE_EOF".to_string(),
        },
        AgentEvent::ToolCompleted {
            run_id: "run-tail".to_string(),
            tool_id: "completed-tool".to_string(),
            status: "success".to_string(),
        },
    ] {
        render_active_agent_event(&mut run, event, &mut Vec::new(), None).unwrap();
    }
    render_active_agent_event(
        &mut run,
        AgentEvent::ToolPermissionRequest {
            run_id: "run-tail".to_string(),
            request_id: "ctrl-tail".to_string(),
            tool_name: "shell".to_string(),
            tool_input: serde_json::json!({"command":"pwd"}),
            tool_use_id: "tool-tail".to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        },
        &mut Vec::new(),
        None,
    )
    .unwrap();
    render_active_agent_event(
        &mut run,
        AgentEvent::AgentFailed {
            run_id: "run-tail".to_string(),
            error: "cosh-core output reached EOF".to_string(),
            error_code: None,
            max_turns: None,
        },
        &mut Vec::new(),
        None,
    )
    .unwrap();
    state.agent_run.active = Some(run);
    let mut output = Vec::new();
    crate::agent::events::render_new_agent_structured_events(&mut state, &mut output, &adapter)
        .unwrap();
    assert_eq!(state.approvals.requests.len(), 1);
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Pending
    );
    assert!(!state.control.shell_handoff().has_active_handoff());
    finish_active_agent_run(&mut state, &mut output, &adapter).unwrap();
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Cancelled
    );
    assert!(state.agent_run.active.is_none());
    assert!(state.agent_run.queued_requests.is_empty());
    assert!(!state.control.shell_handoff().has_active_handoff());
    let completed = state
        .activity
        .tool_invocations
        .iter()
        .find(|tool| tool.invocation_id == "completed-tool")
        .expect("completed activity retained");
    assert_eq!(completed.status, "success");
    assert_eq!(
        completed.output.output_sample,
        "COMPLETED_OUTPUT_BEFORE_EOF"
    );
    assert!(String::from_utf8(output)
        .unwrap()
        .contains("cosh-core output reached EOF"));
}
#[test]
fn failed_run_cleanup_preserves_explicit_handoff_recovery_contract() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut run = active_run(&adapter, "recoverable", Language::EnUs);
    run.request.context_hints = vec![
        crate::types::SHELL_HANDOFF_CONTINUATION_HINT.to_string(),
        "shell handoff recovery owner: approved-1/handoff-1".to_string(),
    ];
    render_active_agent_event(
        &mut run,
        AgentEvent::AgentFailed {
            run_id: "recoverable".to_string(),
            error: "cosh-core output reached EOF".to_string(),
            error_code: None,
            max_turns: None,
        },
        &mut Vec::new(),
        None,
    )
    .unwrap();
    assert!(shell_handoff_resume_fallback_request(&run).is_some());
    assert!(!failed_run_has_no_recovery(&run));
    run.request
        .context_hints
        .push("disable provider resume for shell handoff fallback".to_string());
    assert!(failed_run_has_no_recovery(&run));
}
