use super::*;
use crate::agent::failed_command::failed_command_candidate;
use crate::insight::correlation::MemoryPressureFact;
use crate::insight::model::{
    ExecutionScope, InsightBinding, InsightConfidence, InsightSeverity, InsightTarget,
    OutputExcerptStatus, PromptSuggestion,
};
use crate::recommendation::personal_feedback::FrozenPromptBinding;
use crate::recommendation::personal_model::{
    ActivityPayload, CandidateSource, FeedbackAction, DISCLOSURE_VERSION,
};
use crate::recommendation::personal_runtime::PersonalRuntime;

fn prompt_ghost_event(message: Option<&str>, input: Option<&str>) -> ShellEvent {
    ShellEvent {
        kind: ShellEventKind::UserInputIntercepted,
        session_id: "session-1".to_string(),
        command_id: None,
        command: None,
        cwd: None,
        end_cwd: None,
        exit_code: None,
        started_at_ms: Some(1),
        ended_at_ms: None,
        duration_ms: None,
        terminal_output_ref: None,
        terminal_output_bytes: None,
        input: input.map(str::to_string),
        component: Some("prompt_ghost".to_string()),
        message: message.map(str::to_string),
        command_origin: None,
        shell_environment_generation: None,
        audit_identity: None,
        routing: None,
        capture: None,
    }
}

fn path_prompt_event(input: &str, sensitive: bool) -> ShellEvent {
    let mut event = prompt_ghost_event(Some("input intercepted"), Some(input));
    event.component = Some("path_prompt".to_string());
    event.cwd = Some("/repo".to_string());
    event.routing = sensitive.then_some(crate::types::ShellRoutingMetadata {
        generation: 0,
        top_level_missing: false,
        proven: false,
        sensitive: true,
        unsafe_input: false,
    });
    event
}

#[test]
fn path_prompt_notice_is_stable_and_redacts_sensitive_input() {
    let state = InlineState::default();
    let event = path_prompt_event("read ./README.md", false);
    let request = agent_request_from_intercepted_input(&event, 1, true).expect("plain request");
    let mut output = Vec::new();

    render_path_prompt_notice(&event, &request, &state, &mut output).expect("plain notice");
    let rendered = String::from_utf8(output).expect("UTF-8 notice");
    assert!(rendered.contains("AI request"), "{rendered}");
    assert!(
        rendered.contains("Agent input: read ./README.md"),
        "{rendered}"
    );

    let secret = "read ./README.md --token ghp_abcdefghijklmnopqrstuvwxyz123456";
    let event = path_prompt_event(secret, true);
    let request = agent_request_from_intercepted_input(&event, 2, true).expect("sensitive request");
    let mut output = Vec::new();
    render_path_prompt_notice(&event, &request, &state, &mut output).expect("redacted notice");
    let rendered = String::from_utf8(output).expect("UTF-8 notice");
    assert!(rendered.contains("Agent input: <redacted>"), "{rendered}");
    assert!(!rendered.contains("ghp_"), "{rendered}");
}

#[test]
fn ordinary_natural_language_does_not_render_path_notice() {
    let state = InlineState::default();
    let mut event = path_prompt_event("inspect the current directory", false);
    event.component = Some("natural_language".to_string());
    let request = agent_request_from_intercepted_input(&event, 1, true).expect("plain request");
    let mut output = Vec::new();

    render_path_prompt_notice(&event, &request, &state, &mut output).expect("no notice");
    assert!(output.is_empty());
}

fn candidate_event(candidate_id: &str, message: Option<&str>, input: Option<&str>) -> ShellEvent {
    let mut event = prompt_ghost_event(message, input);
    event.component = Some(format!("prompt_ghost:{candidate_id}"));
    event.started_at_ms = Some(current_time_ms());
    event
}

fn current_time_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

fn personal_binding(candidate_id: &str) -> FrozenPromptBinding {
    FrozenPromptBinding {
        candidate_id: candidate_id.to_string(),
        task_ref: format!("task-{candidate_id}"),
        original_prompt: format!("prompt {candidate_id}"),
        source: CandidateSource::RecentTask,
        suppression_key: format!("suppress-{candidate_id}"),
        profile_generation: 1,
        intent_lifecycle_id: format!("intent-{candidate_id}"),
    }
}

fn feedback_test_state(name: &str) -> (std::path::PathBuf, InlineState) {
    let root = std::env::temp_dir().join(format!(
        "cosh-intercept-feedback-{name}-{}-{}",
        std::process::id(),
        crate::recommendation::personal_crypto::random_hex(6).unwrap()
    ));
    let now = current_time_ms() / 3_600_000;
    let mut runtime = PersonalRuntime::open(true, &root, now).unwrap();
    runtime.mark_notice_seen(DISCLOSURE_VERSION, now).unwrap();
    let writer = runtime.spawn_writer().unwrap();
    let state = InlineState {
        personalization: crate::recommendation::personal_state::PersonalizationState {
            writer: Some(writer),
            ..Default::default()
        },
        ..InlineState::default()
    };
    for _ in 0..100 {
        if state
            .personalization
            .writer
            .as_ref()
            .and_then(|writer| writer.poll_status())
            .is_some()
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    (root, state)
}

fn persisted_feedback_actions(
    root: &std::path::Path,
    state: &mut InlineState,
) -> Vec<(String, FeedbackAction)> {
    let mut writer = state.personalization.writer.take().unwrap();
    let now = current_time_ms() / 3_600_000;
    writer
        .shutdown(now, std::time::Duration::from_secs(1))
        .unwrap();
    PersonalRuntime::open(true, root, now)
        .unwrap()
        .snapshot()
        .unwrap()
        .journal
        .records
        .iter()
        .filter_map(|record| match &record.payload {
            ActivityPayload::RecommendationFeedback {
                candidate_id,
                action,
                ..
            } => Some((candidate_id.clone(), *action)),
            _ => None,
        })
        .collect()
}

#[test]
fn personal_prompt_action_requires_exact_suggestion_id() {
    let mut event = prompt_ghost_event(None, Some("diagnose this repo"));
    event.component = Some("prompt_ghost:candidate-1".to_string());

    assert_eq!(
        personal_prompt_action(&event, "candidate-1"),
        Some(PersonalPromptAction::Submitted("diagnose this repo"))
    );
    assert_eq!(personal_prompt_action(&event, "candidate-2"), None);
}

#[test]
fn personal_prompt_action_distinguishes_accept_and_dismiss() {
    assert_eq!(
        personal_prompt_action(&prompt_ghost_event(Some("accepted"), None), "candidate-1"),
        Some(PersonalPromptAction::Accepted)
    );
    assert_eq!(
        personal_prompt_action(&prompt_ghost_event(Some("dismissed"), None), "candidate-1"),
        Some(PersonalPromptAction::Dismissed)
    );
}

#[test]
fn identified_accept_is_feedback_not_a_standalone_agent_request() {
    let accepted = candidate_event("candidate-1", Some("accepted"), None);
    let submitted = candidate_event("candidate-1", None, Some("prompt candidate-1"));

    assert!(is_prompt_ghost_feedback_event(&accepted));
    assert!(!is_prompt_ghost_feedback_event(&submitted));
}

#[test]
fn selecting_second_personal_prompt_ignores_others_then_submits_selected() {
    let (root, mut state) = feedback_test_state("select-second");
    for id in ["candidate-1", "candidate-2", "candidate-3"] {
        state.pending_prompt_suggestion_bindings.insert(
            id.to_string(),
            PendingInputGhostBinding::Personal(personal_binding(id)),
        );
    }

    handle_personal_prompt_feedback(
        &candidate_event("candidate-2", Some("accepted"), None),
        &mut state,
    );
    handle_personal_prompt_feedback(
        &candidate_event("candidate-2", None, Some("prompt candidate-2")),
        &mut state,
    );

    let actions = persisted_feedback_actions(&root, &mut state);
    assert!(actions.contains(&("candidate-1".to_string(), FeedbackAction::Ignored)));
    assert!(actions.contains(&("candidate-3".to_string(), FeedbackAction::Ignored)));
    assert!(actions.contains(&("candidate-2".to_string(), FeedbackAction::TabAccepted)));
    assert!(actions.contains(&("candidate-2".to_string(), FeedbackAction::Submitted)));
    assert!(state.pending_input_ghost_binding.is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn directly_submitting_personal_prompt_skips_tab_accepted_feedback() {
    let (root, mut state) = feedback_test_state("direct-submit");
    state.pending_prompt_suggestion_bindings.insert(
        "candidate-1".to_string(),
        PendingInputGhostBinding::Personal(personal_binding("candidate-1")),
    );

    handle_personal_prompt_feedback(
        &candidate_event("candidate-1", None, Some("prompt candidate-1")),
        &mut state,
    );

    let actions = persisted_feedback_actions(&root, &mut state);
    assert!(actions.contains(&("candidate-1".to_string(), FeedbackAction::Submitted)));
    assert!(!actions
        .iter()
        .any(|(_, action)| *action == FeedbackAction::TabAccepted));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn shell_exit_marks_all_unresolved_personal_prompts_ignored() {
    let (root, mut state) = feedback_test_state("exit");
    for id in ["candidate-1", "candidate-2"] {
        state.pending_prompt_suggestion_bindings.insert(
            id.to_string(),
            PendingInputGhostBinding::Personal(personal_binding(id)),
        );
    }
    let mut exit = prompt_ghost_event(None, None);
    exit.kind = ShellEventKind::ShellExited;
    exit.component = None;
    exit.started_at_ms = None;
    exit.ended_at_ms = Some(current_time_ms());

    finalize_personal_prompt_feedback_on_exit(&exit, &mut state);

    let actions = persisted_feedback_actions(&root, &mut state);
    assert_eq!(
        actions
            .iter()
            .filter(|(_, action)| *action == FeedbackAction::Ignored)
            .count(),
        2
    );
    assert!(state.pending_prompt_suggestion_bindings.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn stopping_personal_recommendations_ignores_personal_and_preserves_health_binding() {
    let (root, mut state) = feedback_test_state("stop-personal");
    for id in ["candidate-1", "candidate-2"] {
        state.pending_prompt_suggestion_bindings.insert(
            id.to_string(),
            PendingInputGhostBinding::Personal(personal_binding(id)),
        );
    }
    state.pending_prompt_suggestion_bindings.insert(
        "health-1".to_string(),
        PendingInputGhostBinding::Health(AgentContextBinding::StartupHealthFollowUp),
    );

    let mut event = prompt_ghost_event(None, None);
    event.started_at_ms = Some(current_time_ms());
    finalize_unresolved_personal_prompt_feedback(&event, &mut state);

    let actions = persisted_feedback_actions(&root, &mut state);
    assert_eq!(
        actions
            .iter()
            .filter(|(_, action)| *action == FeedbackAction::Ignored)
            .count(),
        2
    );
    assert_eq!(state.pending_prompt_suggestion_bindings.len(), 1);
    assert!(matches!(
        state.pending_prompt_suggestion_bindings.get("health-1"),
        Some(PendingInputGhostBinding::Health(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn health_prompt_selection_does_not_create_personal_feedback() {
    let (root, mut state) = feedback_test_state("health");
    state.pending_prompt_suggestion_bindings.insert(
        "health-1".to_string(),
        PendingInputGhostBinding::Health(AgentContextBinding::StartupHealthFollowUp),
    );

    handle_personal_prompt_feedback(
        &candidate_event("health-1", Some("accepted"), None),
        &mut state,
    );

    assert!(persisted_feedback_actions(&root, &mut state).is_empty());
    assert!(matches!(
        state.pending_prompt_suggestion_bindings.get("health-1"),
        Some(PendingInputGhostBinding::Health(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn selecting_health_prompt_ignores_all_visible_personal_prompts() {
    let (root, mut state) = feedback_test_state("health-with-personal");
    state.pending_prompt_suggestion_bindings.insert(
        "health-1".to_string(),
        PendingInputGhostBinding::Health(AgentContextBinding::StartupHealthFollowUp),
    );
    for id in ["candidate-1", "candidate-2"] {
        state.pending_prompt_suggestion_bindings.insert(
            id.to_string(),
            PendingInputGhostBinding::Personal(personal_binding(id)),
        );
    }

    handle_personal_prompt_feedback(
        &candidate_event("health-1", None, Some("inspect health")),
        &mut state,
    );

    let actions = persisted_feedback_actions(&root, &mut state);
    assert_eq!(
        actions
            .iter()
            .filter(|(_, action)| *action == FeedbackAction::Ignored)
            .count(),
        2
    );
    assert!(matches!(
        state.pending_prompt_suggestion_bindings.get("health-1"),
        Some(PendingInputGhostBinding::Health(_))
    ));
    let _ = std::fs::remove_dir_all(root);
}

fn source_block() -> CommandBlock {
    CommandBlock {
        id: "cmd-1".to_string(),
        session_id: "session-1".to_string(),
        command: "cargo test".to_string(),
        origin: Default::default(),
        cwd: "/tmp".to_string(),
        end_cwd: "/tmp".to_string(),
        started_at_ms: 1,
        ended_at_ms: 2,
        duration_ms: 1,
        exit_code: 1,
        status: CommandStatus::Failed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    }
}

fn insight_binding(suggestion_id: &str) -> InsightBinding {
    InsightBinding {
        suggestion_id: suggestion_id.to_string(),
        target: InsightTarget {
            insight_id: "insight-1".to_string(),
            source_session_id: "session-1".to_string(),
            source_command_block_id: "cmd-1".to_string(),
            scope: ExecutionScope::local("session-1"),
            evidence_handle: None,
            evidence_status: OutputExcerptStatus::Available,
            severity: crate::insight::model::InsightSeverity::Warning,
            confidence: crate::insight::model::InsightConfidence::High,
            evidence: Vec::new(),
            created_at_ms: 1,
        },
    }
}

#[test]
fn dismissed_prompt_ghost_clears_pending_binding() {
    let mut state = InlineState {
        pending_input_ghost_binding: Some(PendingInputGhostBinding::Health(
            AgentContextBinding::StartupHealthFollowUp,
        )),
        ..Default::default()
    };

    clear_dismissed_prompt_ghost_context(&prompt_ghost_event(Some("dismissed"), None), &mut state);

    assert!(state.pending_input_ghost_binding.is_none());
}

#[test]
fn accepted_prompt_ghost_does_not_clear_pending_binding_before_binding() {
    let mut state = InlineState {
        pending_input_ghost_binding: Some(PendingInputGhostBinding::Health(
            AgentContextBinding::StartupHealthFollowUp,
        )),
        ..Default::default()
    };

    clear_dismissed_prompt_ghost_context(
        &prompt_ghost_event(
            Some("input intercepted before reaching bash"),
            Some("analyze"),
        ),
        &mut state,
    );

    assert!(state.pending_input_ghost_binding.is_some());
}

#[test]
fn matching_insight_suggestion_consumes_binding_once_and_uses_source_block() {
    let block = source_block();
    let mut state = InlineState {
        pending_input_ghost_binding: Some(PendingInputGhostBinding::Insight(Box::new(
            insight_binding("suggestion-1"),
        ))),
        ..Default::default()
    };
    let mut event = prompt_ghost_event(
        Some("input intercepted before reaching bash"),
        Some("analyze edited failure"),
    );
    event.component = Some("prompt_ghost:suggestion-1".to_string());

    let request =
        agent_request_from_pending_insight(&event, std::slice::from_ref(&block), &mut state)
            .expect("matching bound request");

    assert_eq!(request.command_block.id, block.id);
    assert_eq!(
        request.user_input.as_deref(),
        Some("analyze edited failure")
    );
    assert!(state.pending_input_ghost_binding.is_none());
    assert!(
        agent_request_from_pending_insight(&event, std::slice::from_ref(&block), &mut state,)
            .is_none()
    );
}

#[test]
fn mismatched_suggestion_clears_binding_and_falls_back_without_history() {
    let block = source_block();
    let mut state = InlineState {
        pending_input_ghost_binding: Some(PendingInputGhostBinding::Insight(Box::new(
            insight_binding("new-suggestion"),
        ))),
        ..Default::default()
    };
    let mut event = prompt_ghost_event(
        Some("input intercepted before reaching bash"),
        Some("analyze visible text"),
    );
    event.component = Some("prompt_ghost:old-suggestion".to_string());

    assert!(
        agent_request_from_pending_insight(&event, std::slice::from_ref(&block), &mut state,)
            .is_none()
    );
    assert!(state.pending_input_ghost_binding.is_none());

    let fallback =
        agent_request_from_intercepted_input(&event, 1, true).expect("free-form fallback");
    assert_eq!(
        crate::types::request_context_binding(&fallback),
        AgentContextBinding::FreeForm
    );
    assert!(fallback.findings.is_empty());
    assert!(fallback.context_blocks.is_empty());
}

#[test]
fn missing_source_block_clears_binding_without_cross_binding() {
    let mut state = InlineState {
        pending_input_ghost_binding: Some(PendingInputGhostBinding::Insight(Box::new(
            insight_binding("suggestion-1"),
        ))),
        ..Default::default()
    };
    let mut event = prompt_ghost_event(
        Some("input intercepted before reaching bash"),
        Some("analyze visible text"),
    );
    event.component = Some("prompt_ghost:suggestion-1".to_string());

    assert!(agent_request_from_pending_insight(&event, &[], &mut state).is_none());
    assert!(state.pending_input_ghost_binding.is_none());
}

#[test]
fn session_mismatch_clears_binding_without_cross_binding() {
    let block = source_block();
    let mut state = InlineState {
        pending_input_ghost_binding: Some(PendingInputGhostBinding::Insight(Box::new(
            insight_binding("suggestion-1"),
        ))),
        ..Default::default()
    };
    let mut event = prompt_ghost_event(
        Some("input intercepted before reaching bash"),
        Some("analyze visible text"),
    );
    event.session_id = "other-session".to_string();
    event.component = Some("prompt_ghost:suggestion-1".to_string());

    assert!(
        agent_request_from_pending_insight(&event, std::slice::from_ref(&block), &mut state,)
            .is_none()
    );
    assert!(state.pending_input_ghost_binding.is_none());
}

#[test]
fn memory_evidence_includes_recent_provider_safe_facts_not_boolean_marker() {
    let mut block = source_block();
    block.command = "ps aux".to_string();
    block.exit_code = 0;
    block.status = CommandStatus::Completed;
    block.ended_at_ms = 2_000;
    let mut request = AgentRequest {
        id: "request-1".to_string(),
        session_id: block.session_id.clone(),
        command_block: block,
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: Some("analyze memory".to_string()),
        findings: Vec::new(),
        mode: AgentMode::AnalysisOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    let mut state = InlineState::default();
    state.insight_correlation.record(MemoryPressureFact {
        scope: ExecutionScope::local("session-1"),
        ended_at_ms: 1_000,
        severity: InsightSeverity::Warning,
        confidence: InsightConfidence::High,
        source_command_block_id: "cmd-pressure".to_string(),
        provider_safe_fact: "memory_pressure severity=Warning ended_at_ms=1000".to_string(),
    });

    attach_bound_insight_evidence(&mut request, &mut state);

    let evidence = request
        .context_hints
        .iter()
        .find(|hint| hint.starts_with("insight_evidence\n"))
        .expect("insight evidence");
    assert!(evidence.contains(
        "source_command_block_id=cmd-pressure; memory_pressure severity=Warning ended_at_ms=1000"
    ));
    assert!(!evidence.contains("recent_memory_pressure=true"));
}

#[test]
fn smart_and_auto_failure_requests_share_the_same_bounded_evidence() {
    for (command, exit_code, output, expected_profile) in [
        (
            "./demo-script",
            126,
            "bash: ./demo-script: Permission denied\n",
            "failure_profile=permission",
        ),
        (
            "make all",
            2,
            "make: *** [Makefile:2: all] Error 1\n",
            "failure_profile=build_or_test",
        ),
        (
            "python3 demo.py",
            1,
            "Traceback (most recent call last):\n  File \"demo.py\", line 1\nRuntimeError: boom\n",
            "failure_profile=runtime_exception",
        ),
        (
            "./demo-signal",
            139,
            "Segmentation fault (core dumped)\n",
            "failure_profile=abnormal_signal",
        ),
    ] {
        let output_path = std::env::temp_dir().join(format!(
            "cosh-smart-auto-prompt-parity-{}-{exit_code}",
            std::process::id()
        ));
        std::fs::write(&output_path, output).expect("write failure output");
        let mut block = source_block();
        block.command = command.to_string();
        block.exit_code = exit_code;
        block.output.terminal_output_ref = Some(output_path.to_string_lossy().into_owned());
        block.output.terminal_output_bytes = output.len() as u64;
        let candidate = failed_command_candidate(&[], &block).expect("failure insight");
        let PromptSuggestion::AgentPrompt { binding } = candidate.suggestion.expect("agent prompt")
        else {
            panic!("expected agent prompt");
        };
        let mut state = InlineState {
            pending_input_ghost_binding: Some(PendingInputGhostBinding::Insight(binding.clone())),
            ..Default::default()
        };
        let mut event = prompt_ghost_event(None, Some("分析这次失败"));
        event.component = Some(format!("prompt_ghost:{}", binding.suggestion_id));
        let mut smart =
            agent_request_from_pending_insight(&event, std::slice::from_ref(&block), &mut state)
                .expect("smart request");
        attach_bound_insight_evidence(&mut smart, &mut state);

        let mut auto = agent_request_for_auto_failure("session-1", &block, &[]);
        crate::agent::failed_command::attach_failure_evidence_bundle(&mut auto);
        let _ = std::fs::remove_file(output_path);

        let evidence = |request: &AgentRequest| {
            request
                .context_hints
                .iter()
                .find(|hint| hint.starts_with("insight_evidence\n"))
                .cloned()
                .expect("insight evidence")
        };
        let smart_evidence = evidence(&smart);
        assert_eq!(smart_evidence, evidence(&auto));
        assert!(
            smart_evidence.contains(expected_profile),
            "{smart_evidence}"
        );
        assert!(smart.user_confirmed);
        assert!(!auto.user_confirmed);
        assert!(smart.user_input.is_some());
        assert!(auto.user_input.is_none());
    }
}
