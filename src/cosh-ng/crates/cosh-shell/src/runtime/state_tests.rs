use std::time::{Duration, Instant};

use crate::agent::run::PendingAgentRequest;
use crate::hooks::state::HookRuntimeState;
use crate::raw_input::{PromptGhostCandidate, PromptGhostRoute};
use crate::runtime::approval_state::ApprovalState;
use crate::runtime::prelude::{
    AgentMode, AgentRequest, AgentRunOrigin, ApprovalRequestKind, ApprovalRequestStatus,
    CommandBlock, CommandStatus, OutputRefs, ProviderShellRequestKind, RuntimeApprovalRequest,
};
use crate::runtime::state::{
    ActivityState, AgentRunState, AnalysisThrottle, ContinuityState, ControlState, QuestionState,
};
use crate::runtime::state::{InlineState, PendingInputGhostBinding};
use crate::types::AgentContextBinding;

#[test]
fn analysis_throttle_uses_fixed_window_instead_of_sliding_forever() {
    let start = Instant::now();
    let mut throttle = AnalysisThrottle::default();

    assert!(!throttle.should_throttle_at("ps -aux", start));
    assert!(throttle.should_throttle_at("ps -aux", start + Duration::from_secs(1)));
    assert!(throttle.should_throttle_at("ps -aux", start + Duration::from_secs(29)));
    assert!(!throttle.should_throttle_at("ps -aux", start + Duration::from_secs(30)));
    assert!(throttle.should_throttle_at("ps -aux", start + Duration::from_secs(31)));
}

#[test]
fn clearing_personal_candidates_rebuilds_health_first_selection() {
    let mut state = InlineState::default();
    state.pending_prompt_suggestion_bindings.insert(
        "health-1".to_string(),
        PendingInputGhostBinding::Health(AgentContextBinding::StartupHealthFollowUp),
    );
    state.pending_prompt_suggestion_bindings.insert(
        "personal-1".to_string(),
        PendingInputGhostBinding::Personal(
            crate::recommendation::personal_feedback::FrozenPromptBinding {
                candidate_id: "personal-1".to_string(),
                task_ref: "task-1".to_string(),
                original_prompt: "continue deployment".to_string(),
                source: crate::recommendation::personal_model::CandidateSource::RecentTask,
                suppression_key: "suppress-1".to_string(),
                profile_generation: 1,
                intent_lifecycle_id: "intent-1".to_string(),
            },
        ),
    );
    state.pending_input_ghost = Some("inspect memory".to_string());
    state.pending_input_ghost_binding = Some(PendingInputGhostBinding::Health(
        AgentContextBinding::StartupHealthFollowUp,
    ));
    state.pending_input_ghost_route = PromptGhostRoute::AgentSelection {
        candidates: vec![
            PromptGhostCandidate {
                text: "inspect memory".to_string(),
                suggestion_id: "health-1".to_string(),
            },
            PromptGhostCandidate {
                text: "continue deployment".to_string(),
                suggestion_id: "personal-1".to_string(),
            },
        ],
        active: 0,
    };

    assert!(state.clear_personal_prompt_ghost());
    assert_eq!(state.pending_input_ghost.as_deref(), Some("inspect memory"));
    assert_eq!(
        state.pending_input_ghost_route,
        PromptGhostRoute::AgentSelection {
            candidates: vec![PromptGhostCandidate {
                text: "inspect memory".to_string(),
                suggestion_id: "health-1".to_string(),
            }],
            active: 0,
        }
    );
    assert!(!state
        .pending_prompt_suggestion_bindings
        .contains_key("personal-1"));
}

#[test]
fn approval_state_generates_request_ids_from_owned_queue() {
    let mut state = ApprovalState::default();

    assert_eq!(state.next_request_id(), "req-1");
    state.requests.push(RuntimeApprovalRequest {
        id: "req-1".to_string(),
        audit_ref: None,
        run_id: "run-1".to_string(),
        origin: AgentRunOrigin::Standard,
        session_id: "session-1".to_string(),
        cwd: "/tmp".to_string(),
        source: "agent",
        provider_shell_request_kind: ProviderShellRequestKind::StreamedToolCallFallback,
        kind: ApprovalRequestKind::Tool,
        subject: "shell".to_string(),
        preview: "$ pwd".to_string(),
        risk: "medium",
        request_id: None,
        tool_use_id: None,
        tool_input: None,
        original_user_request: None,
        status: ApprovalRequestStatus::Pending,
        execution_path: None,
        command_block_id: None,
        redaction_status: None,
        assessment: None,
        hook_requires_approval: false,
        hook_warnings: Vec::new(),
    });

    assert_eq!(state.next_request_id(), "req-2");
}

#[test]
fn agent_run_state_prioritizes_requests_before_held_text() {
    let mut state = AgentRunState::default();

    state.queue_request(pending_agent_request("normal-1", false));
    state.queue_request(pending_agent_request("before-held", true));
    state.queue_request(pending_agent_request("normal-2", false));

    let queued_ids = state
        .queued_requests
        .iter()
        .map(|pending| pending.request.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(queued_ids, vec!["before-held", "normal-1", "normal-2"]);
}

#[test]
fn agent_run_queue_keeps_runtime_only_origin() {
    let mut state = AgentRunState::default();
    let mut pending = pending_agent_request("insight", false);
    pending.origin = AgentRunOrigin::InsightPrompt;

    state.queue_request(pending);

    assert_eq!(
        state.queued_requests.front().map(|request| request.origin),
        Some(AgentRunOrigin::InsightPrompt)
    );
}

#[test]
fn hook_runtime_state_tracks_blocks_followed_by_user_input() {
    let mut state = HookRuntimeState::default();

    assert!(!state.block_followed_by_user_input("cmd-1"));
    state.mark_block_followed_by_user_input("cmd-1");

    assert!(state.block_followed_by_user_input("cmd-1"));
}

#[test]
fn remaining_runtime_state_owners_keep_their_own_defaults() {
    let activity = ActivityState::default();
    assert!(activity.rows.is_empty());
    assert!(activity.output_dir.is_none());

    let questions = QuestionState::default();
    assert!(questions.items.is_empty());
    assert!(questions.pending_id.is_none());

    let mut control = ControlState::default();
    control.remember_selectable_commands(vec!["echo ok".to_string()], Some(3));
    assert_eq!(control.selectable_command_count(), 1);
    assert_eq!(control.selectable_command(0), Some("echo ok"));
    assert_eq!(control.selectable_commands_available_after(), Some(3));

    let continuity = ContinuityState::default();
    assert!(continuity.facts.items.is_empty());
}

#[test]
fn interactive_shell_handoffs_are_isolated_by_run_and_tool_id() {
    let mut control = ControlState::default();
    assert!(control.record_provider_shell_command_from_tool_call(
        "run-1",
        "tool-1",
        r#"{"command":"sudo first"}"#,
    ));
    control.record_provider_tool_output_delta(
        "run-1",
        "tool-1",
        "stderr",
        "sudo: a terminal is required\n",
    );
    let first = control
        .queue_interactive_shell_handoff_for_tool_failure(
            "run-1",
            "tool-1",
            "error",
            AgentRunOrigin::InsightPrompt,
        )
        .expect("first handoff");

    assert!(control.record_provider_shell_command_from_tool_call(
        "run-2",
        "tool-1",
        r#"{"command":"sudo second"}"#,
    ));
    control.record_provider_tool_output_delta(
        "run-2",
        "tool-1",
        "stderr",
        "sudo: a terminal is required\n",
    );
    let second = control
        .queue_interactive_shell_handoff_for_tool_failure(
            "run-2",
            "tool-1",
            "error",
            AgentRunOrigin::AutoFailure,
        )
        .expect("second handoff");

    assert_ne!(first.id, second.id);
    assert_eq!(second.run_id, "run-2");
    assert_eq!(second.command, "sudo second");
    assert_eq!(second.origin, AgentRunOrigin::AutoFailure);
}

fn pending_agent_request(id: &str, before_held_text: bool) -> PendingAgentRequest {
    PendingAgentRequest {
        request: agent_request(id),
        origin: AgentRunOrigin::Standard,
        intent: crate::agent::run::AgentStartIntent::UserInitiated,
        class: crate::agent::run::PendingRequestClass::Normal,
        selectable_after_event_index: None,
        before_held_text,
    }
}

fn agent_request(id: &str) -> AgentRequest {
    AgentRequest {
        id: id.to_string(),
        session_id: "test-session".to_string(),
        command_block: CommandBlock {
            id: format!("{id}-block"),
            session_id: "test-session".to_string(),
            command: "echo test".to_string(),
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
        context_hints: Vec::new(),
        user_input: Some("test".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}
