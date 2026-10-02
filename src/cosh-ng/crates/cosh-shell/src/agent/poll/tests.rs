use std::time::{Duration, Instant};

use super::*;

#[test]
fn foreground_model_tracks_initialization_and_runtime_switch() {
    let initialized = AgentEvent::StatusChanged {
        run_id: "run".to_string(),
        phase: "initialized".to_string(),
        message: "model initialized project-model".to_string(),
    };
    let switched = AgentEvent::StatusChanged {
        run_id: "run".to_string(),
        phase: "model_switched".to_string(),
        message: "model status: model_switched:next-model".to_string(),
    };

    assert_eq!(
        foreground_model_from_event(&initialized),
        Some("project-model")
    );
    assert_eq!(foreground_model_from_event(&switched), Some("next-model"));
}

fn test_active_run() -> ActiveAgentRun {
    let request = AgentRequest {
        id: "request-1".to_string(),
        session_id: "session-1".to_string(),
        command_block: CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: "df -h".to_string(),
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
        user_input: Some("df -h".to_string()),
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

fn control_approval_event(run_id: &str, request_id: &str) -> AgentEvent {
    AgentEvent::ToolPermissionRequest {
        run_id: run_id.to_string(),
        request_id: request_id.to_string(),
        tool_name: "run_shell_command".to_string(),
        tool_input: serde_json::json!({ "command": "df -h" }),
        tool_use_id: "toolu-1".to_string(),
        hook_requires_approval: false,
        audit_ref: None,
    }
}

#[test]
fn control_approval_registration_sends_receipt_and_records_ledger() {
    let (approval_tx, approval_rx) = std::sync::mpsc::channel();
    let mut active_run = test_active_run();
    active_run.handle = crate::adapter::AgentRunHandle::test_with_approval_sender(approval_tx);
    let mut ledger = crate::runtime::approval_ledger::ApprovalLifecycleLedger::default();

    register_control_approval_on_first_sight(
        &active_run,
        &mut ledger,
        None,
        &control_approval_event("request-1", "ctrl-1"),
    );

    assert_eq!(ledger.unresponded_for_run("request-1"), vec!["ctrl-1"]);
    match approval_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(crate::adapter::ApprovalChannelMessage::Receipt { request_id }) => {
            assert_eq!(request_id, "ctrl-1");
        }
        other => panic!("expected approval receipt, got {other:?}"),
    }
}

#[test]
fn control_approval_registration_denies_foreign_run_ids_at_the_door() {
    // #1940 fail-closed: a request under a foreign run id can never be
    // drained or swept, so it is rejected on sight — no ledger entry, no
    // receipt (the shell never takes terminal ownership), exactly one
    // terminal deny for the core's waiter.
    let (approval_tx, approval_rx) = std::sync::mpsc::channel();
    let mut active_run = test_active_run();
    active_run.handle = crate::adapter::AgentRunHandle::test_with_approval_sender(approval_tx);
    let mut ledger = crate::runtime::approval_ledger::ApprovalLifecycleLedger::default();
    let audit_root = std::env::temp_dir().join(format!(
        "cosh-poll-foreign-audit-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&audit_root).expect("create audit root");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&audit_root, std::fs::Permissions::from_mode(0o700))
            .expect("private audit root");
    }
    let audit_root = audit_root.canonicalize().expect("canonical audit root");
    let mut audit = crate::journal::audit::ShellAuditRecorder::test_with_root(&audit_root);

    register_control_approval_on_first_sight(
        &active_run,
        &mut ledger,
        Some(&mut audit),
        &control_approval_event("other-run", "ctrl-foreign"),
    );
    drop(audit);

    assert!(
        ledger.unresponded_for_run("other-run").is_empty(),
        "a foreign request must never enter the ledger"
    );
    match approval_rx.recv_timeout(Duration::from_secs(1)) {
        Ok(crate::adapter::ApprovalChannelMessage::Response(response)) => {
            assert_eq!(response.request_id, "ctrl-foreign");
            match response.decision {
                crate::adapter::ApprovalDecision::Deny { ref message } => {
                    assert_eq!(
                        message,
                        crate::approval::runtime::FOREIGN_RUN_REQUEST_DENY_MESSAGE
                    );
                }
                other => panic!("expected a terminal deny, got {other:?}"),
            }
        }
        other => panic!("expected a terminal deny, got {other:?}"),
    }
    assert!(
        approval_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "no receipt may follow the deny"
    );

    // The door rejection is auditable under its own drop site.
    let mut audit_text = String::new();
    for date in std::fs::read_dir(audit_root.join("v1/segments")).expect("audit segments") {
        for file in std::fs::read_dir(date.expect("date dir").path()).expect("segment files") {
            audit_text.push_str(
                &std::fs::read_to_string(file.expect("segment file").path()).expect("segment text"),
            );
        }
    }
    assert!(
        audit_text.contains("foreign_run_rejected"),
        "the door rejection must be auditable: {audit_text}"
    );
    let _ = std::fs::remove_dir_all(&audit_root);
}

#[test]
fn reentrant_shell_deny_skips_foreign_run_ids() {
    // #1940: the registration door already sent the terminal deny for a
    // foreign-run request; the recovery-path reentrant deny must not send a
    // second, contradictory response for it.
    let (approval_tx, approval_rx) = std::sync::mpsc::channel();
    let mut active_run = test_active_run();
    active_run.handle = crate::adapter::AgentRunHandle::test_with_approval_sender(approval_tx);

    let denied = deny_reentrant_shell_request_after_foreground_evidence(
        &active_run,
        &control_approval_event("other-run", "ctrl-foreign"),
        true,
    );

    assert!(denied.is_none());
    assert!(
        approval_rx.recv_timeout(Duration::from_millis(50)).is_err(),
        "no second response may be sent for a foreign-run request"
    );
}

#[test]
fn control_approval_registration_ignores_non_approval_events() {
    let (approval_tx, approval_rx) = std::sync::mpsc::channel();
    let mut active_run = test_active_run();
    active_run.handle = crate::adapter::AgentRunHandle::test_with_approval_sender(approval_tx);
    let mut ledger = crate::runtime::approval_ledger::ApprovalLifecycleLedger::default();

    register_control_approval_on_first_sight(
        &active_run,
        &mut ledger,
        None,
        &AgentEvent::StatusChanged {
            run_id: "request-1".to_string(),
            phase: "initialized".to_string(),
            message: "ready".to_string(),
        },
    );

    assert!(ledger.unresponded_for_run("request-1").is_empty());
    assert!(approval_rx.recv_timeout(Duration::from_millis(50)).is_err());
}

#[test]
fn stalled_shell_evidence_delivery_uses_last_activity_idle_time() {
    let mut active_run = test_active_run();
    active_run.started_at = Instant::now() - Duration::from_secs(60);
    active_run.last_activity_at = Instant::now();
    active_run.has_visible_text_delta = true;

    assert!(!active_run_has_stalled_shell_evidence_delivery(&active_run));

    active_run.last_activity_at = Instant::now() - Duration::from_secs(16);

    assert!(active_run_has_stalled_shell_evidence_delivery(&active_run));
}

#[test]
fn shell_evidence_idle_timeout_rejects_zero_and_keeps_valid_values() {
    assert_eq!(
        shell_evidence_idle_timeout_from_config(Some("30")),
        Duration::from_secs(30)
    );
    assert_eq!(
        shell_evidence_idle_timeout_from_config(Some("1")),
        Duration::from_secs(1)
    );

    let default = Duration::from_secs(DEFAULT_SHELL_EVIDENCE_IDLE_TIMEOUT_SECS);
    // #2094: a configured `0` must fall back like a malformed value instead of
    // becoming a zero idle window.
    assert_eq!(shell_evidence_idle_timeout_from_config(Some("0")), default);
    assert_eq!(shell_evidence_idle_timeout_from_config(Some("00")), default);
    for malformed in ["", "-1", "abc", "1.5", "5s", " 5"] {
        assert_eq!(
            shell_evidence_idle_timeout_from_config(Some(malformed)),
            default,
            "malformed value {malformed:?} must fall back to the default"
        );
    }
    assert_eq!(shell_evidence_idle_timeout_from_config(None), default);
}

#[test]
fn resolved_shell_evidence_idle_timeout_is_never_zero() {
    // `active_run_has_stalled_shell_evidence_delivery` compares against this
    // window with `>=`, so a zero window would report every poll of every run
    // as stalled and restart the fallback continuation in a loop.
    for configured in [Some("0"), Some(""), Some("abc"), Some("-1"), None] {
        assert!(
            !shell_evidence_idle_timeout_from_config(configured).is_zero(),
            "configuration {configured:?} must not resolve to a zero idle window"
        );
    }
}

#[test]
fn stalled_shell_fallback_waits_for_pending_interaction_to_close() {
    assert!(!should_start_stalled_provider_shell_fallback(
        StalledProviderShellFallbackInputs {
            active_run_idle: true,
            pending_interaction: true,
            ..StalledProviderShellFallbackInputs::default()
        }
    ));
    assert!(!should_start_stalled_provider_shell_fallback(
        StalledProviderShellFallbackInputs {
            active_run_idle: true,
            unrendered_interaction: true,
            ..StalledProviderShellFallbackInputs::default()
        }
    ));
    assert!(!should_start_stalled_provider_shell_fallback(
        StalledProviderShellFallbackInputs {
            active_run_idle: true,
            queued_before_held_text: true,
            ..StalledProviderShellFallbackInputs::default()
        }
    ));
}

#[test]
fn stalled_shell_fallback_starts_only_when_idle_and_clear() {
    assert!(!should_start_stalled_provider_shell_fallback(
        StalledProviderShellFallbackInputs::default()
    ));
    assert!(!should_start_stalled_provider_shell_fallback(
        StalledProviderShellFallbackInputs {
            active_run_idle: true,
            provider_shell_activity_pending: true,
            ..StalledProviderShellFallbackInputs::default()
        }
    ));
    assert!(should_start_stalled_provider_shell_fallback(
        StalledProviderShellFallbackInputs {
            active_run_idle: true,
            ..StalledProviderShellFallbackInputs::default()
        }
    ));
}

#[test]
fn shell_evidence_progress_includes_tool_events() {
    assert!(shell_evidence_provider_progress_observed(
        &AgentEvent::ToolCall {
            run_id: "run-1".to_string(),
            tool_id: Some("tool-1".to_string()),
            name: "run_shell_command".to_string(),
            input: "df -h".to_string(),
        }
    ));
    assert!(shell_evidence_provider_progress_observed(
        &AgentEvent::ToolOutputDelta {
            run_id: "run-1".to_string(),
            tool_id: "tool-1".to_string(),
            stream: "stdout".to_string(),
            text: "output".to_string(),
        }
    ));
    assert!(shell_evidence_provider_progress_observed(
        &AgentEvent::ToolCompleted {
            run_id: "run-1".to_string(),
            tool_id: "tool-1".to_string(),
            status: "success".to_string(),
        }
    ));
}

#[test]
fn shell_evidence_duplicate_signature_distinguishes_list_command_pages() {
    let first =
        shell_evidence_action_signature(&crate::adapter::ShellEvidenceAction::ListCommands {
            limit: 20,
            cursor: None,
        });
    let same_first =
        shell_evidence_action_signature(&crate::adapter::ShellEvidenceAction::ListCommands {
            limit: 20,
            cursor: None,
        });
    let second_page =
        shell_evidence_action_signature(&crate::adapter::ShellEvidenceAction::ListCommands {
            limit: 20,
            cursor: Some("cursor-2".to_string()),
        });

    assert_eq!(first, same_first);
    assert_ne!(first, second_page);
}

#[test]
fn shell_evidence_duplicate_signature_ignores_read_output_bypass() {
    let normal =
        shell_evidence_action_signature(&crate::adapter::ShellEvidenceAction::ReadOutput {
            output_id: "terminal-output://session-1/cmd-1".to_string(),
            direction: crate::adapter::ShellOutputDirection::Tail,
            lines: 120,
            bypass_recent_filter: false,
        });
    let bypass =
        shell_evidence_action_signature(&crate::adapter::ShellEvidenceAction::ReadOutput {
            output_id: "terminal-output://session-1/cmd-1".to_string(),
            direction: crate::adapter::ShellOutputDirection::Tail,
            lines: 120,
            bypass_recent_filter: true,
        });

    assert_eq!(normal, bypass);
}

#[test]
fn text_hold_reason_none_for_plain_text_streaming() {
    assert_eq!(text_hold_reason_for_poll(TextHoldInputs::default()), None);
}

#[test]
fn text_hold_reason_separates_interaction_and_post_tool_holds() {
    assert_eq!(
        text_hold_reason_for_poll(TextHoldInputs {
            pending_interaction_before_poll: true,
            provider_native_shell_result_pending: true,
            ..TextHoldInputs::default()
        }),
        Some(TextHoldReason::InteractionPending)
    );
    assert_eq!(
        text_hold_reason_for_poll(TextHoldInputs {
            provider_native_shell_result_pending: true,
            ..TextHoldInputs::default()
        }),
        Some(TextHoldReason::PostToolShellResult)
    );
    assert_eq!(
        text_hold_reason_for_poll(TextHoldInputs {
            provider_native_shell_transcript_pending: true,
            ..TextHoldInputs::default()
        }),
        Some(TextHoldReason::PostToolShellTranscript)
    );
}
