use super::evidence_requests::*;
use super::prelude::*;
use crate::agent::run::ActiveAgentRun;
use crate::evidence::model::{EvidenceExcerptRequest, OutputExcerptDirection};
use crate::evidence::request::{CoshRequest, ParsedCoshRequest};
use crate::types::{request_context_binding, AgentContextBinding};

#[test]
fn output_request_injects_bounded_excerpt_without_path() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-evidence-request-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let output_ref = dir.join("cmd-1.txt");
    std::fs::write(&output_ref, "one\ntwo\nthree\n").expect("write output");
    let mut block = command_block(output_ref.to_str().expect("utf8 output path"));
    block.command = "curl --token cli-secret https://example.test/?secret=query-secret".to_string();
    let request = RuntimeEvidenceRequest {
        id: "evidence-1".to_string(),
        origin: AgentRunOrigin::Standard,
        kind: RuntimeEvidenceRequestKind::Output(EvidenceExcerptRequest {
            output_id: "terminal-output://session-1/cmd-1".to_string(),
            direction: OutputExcerptDirection::Tail,
            lines: Some(2),
        }),
        ignored_multiple_request_blocks: false,
        audit_id: None,
        audit_ref: None,
    };

    let agent_request =
        agent_request_from_evidence_request(&[block], &request, 9).expect("agent request");

    assert_eq!(agent_request.id, "evidence-output-9");
    let input = agent_request.user_input.expect("user input");
    assert!(input.starts_with("ShellEvidenceExcerpt\n"), "{input}");
    assert!(
        input.contains("output_id: terminal-output://session-1/cmd-1"),
        "{input}"
    );
    assert!(
        input.contains("bounded_output_excerpt:\ntwo\nthree"),
        "{input}"
    );
    assert!(
        input.contains("output_excerpt_status: available"),
        "{input}"
    );
    assert!(!input.contains(output_ref.to_str().unwrap()), "{input}");
}

#[test]
fn output_request_marks_capture_truncated_status() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-evidence-request-truncated-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let output_ref = dir.join("cmd-1.txt");
    std::fs::write(&output_ref, "one\ntwo\nthree\n").expect("write output");
    let mut block = command_block(output_ref.to_str().expect("utf8 output path"));
    block.output.terminal_output_bytes = COMMAND_OUTPUT_REF_MAX_BYTES as u64 + 1;
    let request = RuntimeEvidenceRequest {
        id: "evidence-1".to_string(),
        origin: AgentRunOrigin::Standard,
        kind: RuntimeEvidenceRequestKind::Output(EvidenceExcerptRequest {
            output_id: "terminal-output://session-1/cmd-1".to_string(),
            direction: OutputExcerptDirection::Tail,
            lines: Some(2),
        }),
        ignored_multiple_request_blocks: false,
        audit_id: None,
        audit_ref: None,
    };

    let agent_request =
        agent_request_from_evidence_request(&[block], &request, 9).expect("agent request");
    let input = agent_request.user_input.expect("user input");

    assert!(
        input.contains("output_excerpt_status: truncated_at_capture"),
        "{input}"
    );
    assert!(
        input.contains("bounded_output_excerpt:\ntwo\nthree"),
        "{input}"
    );
}

#[test]
fn history_request_injects_index_without_output_contents() {
    let block = command_block("/tmp/missing-output");
    let request = RuntimeEvidenceRequest {
        id: "evidence-1".to_string(),
        origin: AgentRunOrigin::Standard,
        kind: RuntimeEvidenceRequestKind::History,
        ignored_multiple_request_blocks: false,
        audit_id: None,
        audit_ref: None,
    };

    let agent_request =
        agent_request_from_evidence_request(&[block], &request, 3).expect("agent request");
    let input = agent_request.user_input.expect("user input");

    assert!(input.contains("history_index:"), "{input}");
    assert!(
        input.contains("terminal-output://session-1/cmd-1"),
        "{input}"
    );
    assert!(!input.contains("bounded_output_excerpt:"), "{input}");
}

#[test]
fn records_history_request_as_auto_follow_up() {
    let mut state = InlineState::default();
    state.session_blocks = vec![command_block("/tmp/missing-output")];
    let mut active_run = test_active_run();
    active_run.origin = AgentRunOrigin::AutoFailure;
    active_run.pending_cosh_requests = vec![ParsedCoshRequest {
        request: CoshRequest::History,
        ignored_multiple_request_blocks: false,
    }];
    active_run.pending_cosh_request_audits =
        vec![crate::evidence::stream::CoshRequestAuditRecord {
            raw_block: "```cosh-request\nhistory\n```".to_string(),
            outcome: crate::evidence::stream::CoshRequestAuditOutcome::Parsed,
            reason: "parsed",
        }];

    let recorded = record_cosh_requests_from_active_run(&mut state, &mut active_run);

    assert_eq!(recorded.auto_requests.len(), 1);
    assert_eq!(recorded.card_ids.len(), 0);
    assert_eq!(state.evidence_requests.pending.len(), 0);
    let (request, origin) = &recorded.auto_requests[0];
    assert_eq!(*origin, AgentRunOrigin::AutoFailure);
    let input = request.user_input.as_deref().expect("history input");
    assert!(input.contains("history_index:"), "{input}");
    assert!(
        input.contains("terminal-output://session-1/cmd-1"),
        "{input}"
    );
    assert_eq!(state.evidence_requests.audit_records.len(), 1);
    assert_eq!(
        state.evidence_requests.audit_records[0].id,
        "cosh-request-1"
    );
    assert_eq!(
        state.evidence_requests.audit_records[0].raw_block,
        "```cosh-request\nhistory\n```"
    );
}

#[test]
fn records_history_request_as_card_when_command_redacts() {
    let mut state = InlineState::default();
    let mut block = command_block("/tmp/missing-output");
    block.command = "echo token=super-secret".to_string();
    state.session_blocks = vec![block];
    let mut active_run = test_active_run();
    active_run.origin = AgentRunOrigin::InsightPrompt;
    active_run.pending_cosh_requests = vec![ParsedCoshRequest {
        request: CoshRequest::History,
        ignored_multiple_request_blocks: false,
    }];

    let recorded = record_cosh_requests_from_active_run(&mut state, &mut active_run);

    assert!(recorded.auto_requests.is_empty());
    assert_eq!(recorded.card_ids, vec!["evidence-1"]);
    assert_eq!(state.evidence_requests.pending.len(), 1);
    assert_eq!(
        state.evidence_requests.pending[0].origin,
        AgentRunOrigin::InsightPrompt
    );
}

#[test]
fn records_history_request_as_card_in_recommend_mode() {
    let mut state = InlineState {
        approval_mode: CoshApprovalMode::Recommend,
        ..InlineState::default()
    };
    state.session_blocks = vec![command_block("/tmp/missing-output")];
    let mut active_run = test_active_run();
    active_run.pending_cosh_requests = vec![ParsedCoshRequest {
        request: CoshRequest::History,
        ignored_multiple_request_blocks: false,
    }];

    let recorded = record_cosh_requests_from_active_run(&mut state, &mut active_run);

    assert!(recorded.auto_requests.is_empty());
    assert_eq!(recorded.card_ids, vec!["evidence-1"]);
    assert_eq!(state.evidence_requests.pending.len(), 1);
}

#[test]
fn does_not_start_follow_up_when_provider_tool_turn_is_open() {
    let mut state = InlineState::default();
    state.session_blocks = vec![command_block("/tmp/missing-output")];
    let mut active_run = test_active_run();
    active_run
        .governed_events
        .push(governed(AgentEvent::ToolCall {
            run_id: "request-1".to_string(),
            tool_id: Some("toolu-open".to_string()),
            name: "Read".to_string(),
            input: "{\"file_path\":\"README.md\"}".to_string(),
        }));
    active_run.pending_cosh_requests = vec![ParsedCoshRequest {
        request: CoshRequest::History,
        ignored_multiple_request_blocks: false,
    }];

    let recorded = record_cosh_requests_from_active_run(&mut state, &mut active_run);

    assert!(recorded.auto_requests.is_empty());
    assert!(recorded.card_ids.is_empty());
    assert_eq!(state.evidence_requests.pending.len(), 0);
    assert!(
        recorded
            .notices
            .iter()
            .any(|notice| notice.contains("provider tool turn is still open")),
        "{:?}",
        recorded.notices
    );
}

#[test]
fn closed_provider_tool_turn_allows_history_follow_up() {
    let mut state = InlineState::default();
    state.session_blocks = vec![command_block("/tmp/missing-output")];
    let mut active_run = test_active_run();
    active_run
        .governed_events
        .push(governed(AgentEvent::ToolCall {
            run_id: "request-1".to_string(),
            tool_id: Some("toolu-closed".to_string()),
            name: "Read".to_string(),
            input: "{\"file_path\":\"README.md\"}".to_string(),
        }));
    active_run
        .governed_events
        .push(governed(AgentEvent::ToolCompleted {
            run_id: "request-1".to_string(),
            tool_id: "toolu-closed".to_string(),
            status: "completed".to_string(),
        }));
    active_run.pending_cosh_requests = vec![ParsedCoshRequest {
        request: CoshRequest::History,
        ignored_multiple_request_blocks: false,
    }];

    let recorded = record_cosh_requests_from_active_run(&mut state, &mut active_run);

    assert_eq!(recorded.auto_requests.len(), 1);
    assert!(recorded.card_ids.is_empty());
    assert!(recorded.notices.is_empty());
}

#[test]
fn records_invalid_request_block_audit_without_evidence_request() {
    let mut state = InlineState::default();
    let mut active_run = test_active_run();
    active_run.pending_cosh_request_audits =
        vec![crate::evidence::stream::CoshRequestAuditRecord {
            raw_block: "```cosh-request\nread /tmp/out\n```".to_string(),
            outcome: crate::evidence::stream::CoshRequestAuditOutcome::Invalid,
            reason: "parse_error",
        }];

    let recorded = record_cosh_requests_from_active_run(&mut state, &mut active_run);

    assert!(recorded.auto_requests.is_empty());
    assert!(recorded.card_ids.is_empty());
    assert_eq!(state.evidence_requests.audit_records.len(), 1);

    let mut output = Vec::new();
    crate::runtime::details::render_runtime_details(&state, &[], "cosh-request-1", &mut output)
        .expect("render details");
    let rendered = String::from_utf8(output).expect("utf8 details");
    assert!(rendered.contains("cosh-request details"), "{rendered}");
    assert!(rendered.contains("outcome: invalid"), "{rendered}");
    assert!(rendered.contains("reason: parse_error"), "{rendered}");
    assert!(rendered.contains("read /tmp/out"), "{rendered}");
}

#[test]
fn evidence_follow_ups_keep_session_and_plain_user_payload() {
    let dir = std::env::temp_dir().join(format!(
        "cosh-shell-evidence-follow-up-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    let output_ref = dir.join("cmd-1.txt");
    std::fs::write(&output_ref, "one\ntwo\nthree\n").expect("write output");
    let mut block = command_block(output_ref.to_str().expect("utf8 output path"));
    block.command = "curl --token cli-secret https://example.test/?secret=query-secret".to_string();
    let history_request = RuntimeEvidenceRequest {
        id: "evidence-1".to_string(),
        origin: AgentRunOrigin::Standard,
        kind: RuntimeEvidenceRequestKind::History,
        ignored_multiple_request_blocks: false,
        audit_id: None,
        audit_ref: None,
    };
    let output_request = RuntimeEvidenceRequest {
        id: "evidence-2".to_string(),
        origin: AgentRunOrigin::Standard,
        kind: RuntimeEvidenceRequestKind::Output(EvidenceExcerptRequest {
            output_id: "terminal-output://session-1/cmd-1".to_string(),
            direction: OutputExcerptDirection::Tail,
            lines: Some(2),
        }),
        ignored_multiple_request_blocks: false,
        audit_id: None,
        audit_ref: None,
    };

    let history =
        agent_request_from_evidence_request(std::slice::from_ref(&block), &history_request, 1)
            .expect("history follow-up");
    let output =
        agent_request_from_evidence_request(std::slice::from_ref(&block), &output_request, 2)
            .expect("output follow-up");

    for request in [history, output] {
        assert_eq!(request.session_id, "session-1");
        assert_eq!(request.command_block.id, "cmd-1");
        assert_eq!(request.mode, AgentMode::RecommendOnly);
        assert!(request.user_confirmed);
        assert!(request.context_blocks.is_empty());
        assert_eq!(
            request_context_binding(&request),
            AgentContextBinding::ControlProtocolEvidence
        );
        let input = request.user_input.as_deref().expect("plain user input");
        assert!(input.starts_with("ShellEvidenceExcerpt\n"), "{input}");
        assert!(input.contains("command:"), "{input}");
        assert!(input.contains("--token <redacted>"), "{input}");
        assert!(input.contains("secret=<redacted>"), "{input}");
        assert!(!input.contains("cli-secret"), "{input}");
        assert!(!input.contains("query-secret"), "{input}");
        assert!(!input.contains("tool_result"), "{input}");
        assert!(!input.contains("host_executed_shell"), "{input}");
        assert!(!input.contains("control_response"), "{input}");
    }
}

#[test]
fn records_only_first_output_request_from_provider_response() {
    let mut state = InlineState::default();
    let mut active_run = test_active_run();
    active_run.pending_cosh_requests = vec![
        ParsedCoshRequest {
            request: CoshRequest::Output(EvidenceExcerptRequest {
                output_id: "terminal-output://session-1/cmd-1".to_string(),
                direction: OutputExcerptDirection::Tail,
                lines: None,
            }),
            ignored_multiple_request_blocks: false,
        },
        ParsedCoshRequest {
            request: CoshRequest::Output(EvidenceExcerptRequest {
                output_id: "terminal-output://session-1/cmd-2".to_string(),
                direction: OutputExcerptDirection::Head,
                lines: None,
            }),
            ignored_multiple_request_blocks: false,
        },
    ];

    let recorded = record_cosh_requests_from_active_run(&mut state, &mut active_run);

    assert!(recorded.auto_requests.is_empty());
    assert_eq!(recorded.card_ids, vec!["evidence-1".to_string()]);
    assert_eq!(state.evidence_requests.pending.len(), 1);
    assert!(matches!(
        state.evidence_requests.pending[0].kind,
        RuntimeEvidenceRequestKind::Output(_)
    ));
    assert!(state.evidence_requests.pending[0].ignored_multiple_request_blocks);
}

#[test]
fn clear_pending_evidence_requests_drops_pending_cards() {
    let mut state = InlineState::default();
    let mut active_run = test_active_run();
    active_run.pending_cosh_requests = vec![ParsedCoshRequest {
        request: CoshRequest::Output(EvidenceExcerptRequest {
            output_id: "terminal-output://session-1/cmd-1".to_string(),
            direction: OutputExcerptDirection::Tail,
            lines: None,
        }),
        ignored_multiple_request_blocks: false,
    }];

    let recorded = record_cosh_requests_from_active_run(&mut state, &mut active_run);
    assert_eq!(recorded.card_ids, vec!["evidence-1".to_string()]);
    state
        .evidence_requests
        .rendered
        .insert("evidence-1".to_string());

    clear_pending_evidence_requests(&mut state);

    assert!(state.evidence_requests.pending.is_empty());
    assert!(state.evidence_requests.rendered.is_empty());
}

fn command_block(output_ref: &str) -> CommandBlock {
    CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "session-1".to_string(),
        command: "printf 'one\\ntwo\\nthree\\n'".to_string(),
        origin: Default::default(),
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        status: CommandStatus::Completed,
        exit_code: 0,
        duration_ms: 10,
        output: OutputRefs {
            terminal_output_ref: Some(output_ref.to_string()),
            terminal_output_bytes: 14,
        },
        started_at_ms: 1,
        ended_at_ms: 11,
        shell_environment_generation: None,
        audit_identity: None,
    }
}

fn test_active_run() -> ActiveAgentRun {
    let request = AgentRequest {
        id: "request-1".to_string(),
        session_id: "session-1".to_string(),
        command_block: command_block("/tmp/missing-output"),
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
        started_at: std::time::Instant::now(),
        last_activity_at: std::time::Instant::now(),
        last_heartbeat_at: std::time::Instant::now(),
        current_phase: String::new(),
        current_message: String::new(),
        has_visible_text_delta: false,
        completed: false,
        host_completed_tool_ids: Vec::new(),
        pending_hook_notifications: Vec::new(),
    }
}

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

fn evidence_send_event(request_id: &str) -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("session-1", request_id);
    event.component = Some("card".to_string());
    event.input = Some(request_id.to_string());
    event.message = Some("evidence_send".to_string());
    event
}

#[test]
fn evidence_send_restores_card_when_queue_is_full() {
    use crate::adapter::{AdapterInstance, CoshCoreAdapter};
    use crate::agent::queue::MAX_QUEUED_AGENT_REQUESTS;
    use crate::agent::run::{AgentRunOrigin, AgentStartIntent, PendingAgentRequest};

    let mut state = InlineState::default();
    // An active run makes the gate queue new requests; fill the queue so the
    // next enqueue is rejected. (No compaction here — this is the ordinary
    // busy-Agent path.)
    state.agent_run.active = Some(test_active_run());
    state.session_blocks = vec![command_block("/tmp/missing-output")];
    let filler = agent_request_from_evidence_request(
        &state.session_blocks,
        &RuntimeEvidenceRequest {
            id: "filler".to_string(),
            origin: AgentRunOrigin::Standard,
            kind: RuntimeEvidenceRequestKind::History,
            ignored_multiple_request_blocks: false,
            audit_id: None,
            audit_ref: None,
        },
        0,
    )
    .expect("filler request");
    for _ in 0..MAX_QUEUED_AGENT_REQUESTS {
        state
            .agent_run
            .queued_requests
            .push_back(PendingAgentRequest {
                request: filler.clone(),
                origin: AgentRunOrigin::Standard,
                intent: AgentStartIntent::UserInitiated,
                class: crate::agent::run::PendingRequestClass::Normal,
                selectable_after_event_index: None,
                before_held_text: false,
            });
    }
    state
        .evidence_requests
        .pending
        .push_back(RuntimeEvidenceRequest {
            id: "evidence-1".to_string(),
            origin: AgentRunOrigin::Standard,
            kind: RuntimeEvidenceRequestKind::History,
            ignored_multiple_request_blocks: false,
            audit_id: None,
            audit_ref: None,
        });

    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let mut output = Vec::new();
    render_evidence_request_actions(
        &[evidence_send_event("evidence-1")],
        &state.session_blocks.clone(),
        &adapter,
        &mut state,
        &mut output,
        0,
    )
    .expect("render evidence actions");

    // The full queue rejected the send, so the consumed card is restored and
    // the rejection is surfaced — the user's action is not lost.
    assert!(state
        .evidence_requests
        .pending
        .iter()
        .any(|request| request.id == "evidence-1"));
    assert_eq!(
        state.agent_run.queued_requests.len(),
        MAX_QUEUED_AGENT_REQUESTS
    );
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("Too many"), "{rendered}");
}

#[test]
fn evidence_send_during_compaction_keeps_the_card_and_shows_paused_notice() {
    use crate::adapter::{AdapterInstance, CoshCoreAdapter};

    let mut state = InlineState::default();
    // A recommended (pending) compaction pauses the Agent conversation.
    crate::slash::session::note_compaction_recommendation(
        &mut state,
        "00000000-0000-4000-8000-000000000000:1:0:200000:100000",
    );
    state
        .evidence_requests
        .pending
        .push_back(RuntimeEvidenceRequest {
            id: "evidence-1".to_string(),
            origin: AgentRunOrigin::Standard,
            kind: RuntimeEvidenceRequestKind::History,
            ignored_multiple_request_blocks: false,
            audit_id: None,
            audit_ref: None,
        });

    // A bogus program proves no model process is launched: the send is
    // intercepted before the pending card is consumed or a run is started.
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let mut output = Vec::new();
    render_evidence_request_actions(
        &[evidence_send_event("evidence-1")],
        &[],
        &adapter,
        &mut state,
        &mut output,
        0,
    )
    .expect("render evidence actions");

    // The pending card is preserved (not consumed), no Agent run started, and
    // the user sees the paused notice so they can send again after compaction.
    assert!(state
        .evidence_requests
        .pending
        .iter()
        .any(|request| request.id == "evidence-1"));
    assert!(state.agent_run.active.is_none());
    assert!(state.agent_run.queued_requests.is_empty());
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("paused"), "{rendered}");
}
