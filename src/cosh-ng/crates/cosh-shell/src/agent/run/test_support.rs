//! Test-only builders for [`ActiveAgentRun`] shared across crate test
//! modules.

use super::*;

/// Builds a minimal active run for tests that need an owner identity. The
/// approval receiver is returned so the caller can keep the channel alive
/// for the test's duration (a dropped receiver fails later responds).
pub(crate) fn test_active_run_with_id(
    run_id: &str,
) -> (
    ActiveAgentRun,
    std::sync::mpsc::Receiver<crate::adapter::ApprovalChannelMessage>,
) {
    let (approval_tx, approval_rx) = std::sync::mpsc::channel();
    let request = AgentRequest {
        id: run_id.to_string(),
        session_id: "session-1".to_string(),
        command_block: CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "session-1".to_string(),
            command: "approval test".to_string(),
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
        user_input: Some("approval test".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    };
    let renderer = RatatuiInlineRenderer::for_terminal();
    (
        ActiveAgentRun {
            request,
            origin: AgentRunOrigin::Standard,
            handle: AgentRunHandle::test_with_approval_sender(approval_tx),
            provider_name: "fake",
            language: Language::EnUs,
            renderer: renderer.clone(),
            status_animation: renderer.status_animation(),
            markdown_stream: renderer.stream_markdown_agent(),
            governed_events: Vec::new(),
            deferred_events: Vec::new(),
            held_events: Vec::new(),
            cosh_request_filter: CoshRequestStreamFilter::default(),
            pending_cosh_requests: Vec::new(),
            pending_cosh_request_audits: Vec::new(),
            pending_hook_notifications: Vec::new(),
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
        },
        approval_rx,
    )
}
