// Owner: activity. Shell handoff activity row builders: the tracked path
// closes pending handoffs against matched command blocks, and the untracked
// fallback closes them at the next prompt boundary with degraded evidence
// when a preexec marker is lost, instead of blocking the Agent run forever.
use crate::runtime::evidence_delivery::record_shell_handoff_completion;
use crate::runtime::prelude::*;

use super::runtime::{
    legacy_activity_summary_message, next_activity_id_excluding, ActivityKind, RuntimeActivityRow,
};

/// Returns the prompt-boundary event that closes the front pending handoff as
/// untracked, when the fallback applies (see close_untracked_shell_handoffs).
fn untracked_handoff_boundary(state: &InlineState, events: &[ShellEvent]) -> Option<ShellEvent> {
    let handoff = state.control.shell_handoff().pending_front()?;
    let emitted_at = handoff.emitted_at_event_index()?;
    // Scan forward from the emission point and let the first relevant
    // boundary decide ownership: a CommandStarted seen before any prompt
    // means the handoff is being tracked normally (the matched-block path
    // owns closure), while a ShellReady seen first closes the handoff as
    // untracked. The event list is cumulative, so commands that run after
    // an already-reached prompt boundary must never veto that boundary.
    let boundary = events.iter().skip(emitted_at).find(|event| {
        matches!(
            event.kind,
            ShellEventKind::ShellReady | ShellEventKind::CommandStarted
        )
    })?;
    if boundary.kind == ShellEventKind::CommandStarted {
        return None;
    }
    Some(boundary.clone())
}

/// Fallback closure for emitted shell handoffs whose preexec marker was lost:
/// once the shell returns to a prompt (`ShellReady`) strictly after the
/// handoff was written to the PTY and no command tracking ever started, the
/// pending handoff must be closed with degraded evidence instead of blocking
/// the Agent run and the input routing forever. The degraded evidence never
/// fabricates an exit code (`-1`) or output.
pub(crate) fn close_untracked_shell_handoffs(
    state: &mut InlineState,
    events: &[ShellEvent],
) -> Vec<String> {
    let mut ids = Vec::new();
    while let Some(ready) = untracked_handoff_boundary(state, events) {
        let handoff = state
            .control
            .shell_handoff_mut()
            .pop_pending()
            .expect("front handoff exists");
        let handoff_request = handoff.request();
        let id = next_shell_handoff_activity_id(state, &handoff_request.approval_id);
        let cwd = ready.cwd.clone().unwrap_or_default();
        let block = CommandBlock {
            id: format!("untracked-{}", handoff_request.approval_id),
            session_id: ready.session_id.clone(),
            // Durable-surface text: the tracked path receives the marker's
            // already-redacted report, but this synthetic block is built from
            // the request's original command, so it must be redacted here or
            // the secret would flow into evidence and the activity detail
            // (#2142 review R5).
            command: crate::evidence::redact_sensitive_text(&handoff_request.command).0,
            origin: expected_handoff_origin(handoff_request),
            cwd: cwd.clone(),
            end_cwd: cwd,
            started_at_ms: 0,
            ended_at_ms: ready.started_at_ms.unwrap_or(0),
            duration_ms: 0,
            // The command may have run untracked or may have been abandoned
            // before execution; both are indistinguishable here, so no real
            // exit code is ever reported.
            exit_code: -1,
            status: CommandStatus::Completed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        };
        state
            .approvals
            .mark_foreground_shell_execution(&handoff_request.approval_id, &block.id);
        state
            .control
            .mark_provider_foreground_shell_command(&block.command);
        let evidence = record_shell_handoff_completion(
            state,
            handoff_request,
            &block,
            crate::types::SHELL_HANDOFF_UNTRACKED_STATUS,
        );
        if let Some(tool_use_id) = handoff_request.tool_use_id.as_deref() {
            state
                .control
                .mark_provider_shell_transcript_seen(&handoff_request.run_id, tool_use_id);
            if let Some(active_run) = state.agent_run.active.as_mut() {
                if active_run.request.id == handoff_request.run_id {
                    active_run.mark_host_completed_tool(tool_use_id);
                }
            }
        } else if let Some(request_id) = handoff_request.request_id.as_deref() {
            if let Some(active_run) = state.agent_run.active.as_mut() {
                if active_run.request.id == handoff_request.run_id {
                    active_run.mark_host_completed_tool(request_id);
                }
            }
        }
        state
            .analyzed_blocks
            .insert(evidence.command_block_id.clone());
        state.activity.rows.push(RuntimeActivityRow {
            id: id.clone(),
            audit_ref: None,
            run_id: handoff_request.run_id.clone(),
            kind: ActivityKind::ShellHandoff,
            status: evidence.status.to_string(),
            subject: evidence.approval_id.clone().unwrap_or_default(),
            summary: legacy_activity_summary_message(
                state,
                MessageId::ActivityShellHandoffSentSummary,
                &[("approval", &handoff_request.approval_id)],
            ),
            detail: format!(
                "evidence: ShellCommandCompleted\napproval: {}\nexecution_path: foreground_shell_pty\nselected_shell_execution_path: {}\npath_selection_reason: preexec_marker_missing; shell returned to prompt without command tracking\nprovider_result_delivery_status: {}\nrecovery_reason: {}\ncommand_block: {}\ncommand: {}\npreview: {}\npreview_hash: {}\nactor: {}\nsource: {}\nrequest_id: {}\ntool_use_id: {}\nstatus: {}\nexit_code: unavailable\nduration_ms: unavailable\nredaction_status: {}\noutput_id: <none>",
                evidence.approval_id.as_deref().unwrap_or("<none>"),
                evidence.selected_execution_path(),
                evidence.provider_result_delivery_status,
                evidence.recovery_reason.unwrap_or("<none>"),
                evidence.command_block_id,
                evidence.command,
                crate::evidence::redact_sensitive_text(&handoff_request.exact_preview).0,
                handoff_request.preview_hash,
                handoff_request.actor,
                handoff_request.source,
                handoff_request.request_id.as_deref().unwrap_or("<none>"),
                handoff_request.tool_use_id.as_deref().unwrap_or("<none>"),
                evidence.status,
                evidence.redaction_status,
            ),
            presentation: None,
        });
        ids.push(id);
    }
    ids
}

pub(crate) fn record_approved_shell_handoff_blocks(
    state: &mut InlineState,
    blocks: &[CommandBlock],
) -> Vec<String> {
    let mut ids = Vec::new();
    while let Some(handoff) = state.control.shell_handoff().pending_front() {
        let request = handoff.request();
        let Some(block) = blocks.iter().find(|block| {
            !state.analyzed_blocks.contains(&block.id)
                && shell_handoff_block_matches_request(block, request)
        }) else {
            break;
        };

        let handoff = state
            .control
            .shell_handoff_mut()
            .pop_pending()
            .expect("front handoff exists");
        let handoff_request = handoff.request();
        let id = next_shell_handoff_activity_id(state, &handoff_request.approval_id);
        let status = classify_shell_handoff_command_outcome(
            block.exit_code,
            &block.command,
            handoff.timeout_interrupt_sent(),
        )
        .status();
        state
            .approvals
            .mark_foreground_shell_execution(&handoff_request.approval_id, &block.id);
        state
            .control
            .mark_provider_foreground_shell_command(&block.command);
        let evidence = record_shell_handoff_completion(state, handoff_request, block, status);
        if let Some(tool_use_id) = handoff_request.tool_use_id.as_deref() {
            state
                .control
                .mark_provider_shell_transcript_seen(&handoff_request.run_id, tool_use_id);
            if let Some(active_run) = state.agent_run.active.as_mut() {
                if active_run.request.id == handoff_request.run_id {
                    active_run.mark_host_completed_tool(tool_use_id);
                }
            }
        } else if let Some(request_id) = handoff_request.request_id.as_deref() {
            if let Some(active_run) = state.agent_run.active.as_mut() {
                if active_run.request.id == handoff_request.run_id {
                    active_run.mark_host_completed_tool(request_id);
                }
            }
        }
        state
            .analyzed_blocks
            .insert(evidence.command_block_id.clone());
        state.activity.rows.push(RuntimeActivityRow {
            id: id.clone(),
            audit_ref: None,
            run_id: handoff_request.run_id.clone(),
            kind: ActivityKind::ShellHandoff,
            status: evidence.status.to_string(),
            subject: evidence.approval_id.clone().unwrap_or_default(),
            summary: legacy_activity_summary_message(
                state,
                MessageId::ActivityShellHandoffSentSummary,
                &[("approval", &handoff_request.approval_id)],
            ),
            detail: format!(
                "evidence: ShellCommandCompleted\napproval: {}\nexecution_path: foreground_shell_pty\nselected_shell_execution_path: {}\npath_selection_reason: {}\nprovider_result_delivery_status: {}\nrecovery_reason: {}\ncommand_block: {}\ncommand: {}\ncwd: {}\nend_cwd: {}\npreview: {}\npreview_hash: {}\nactor: {}\nsource: {}\nrequest_id: {}\ntool_use_id: {}\nstatus: {}\nexit_code: {}\nduration_ms: {}\nredaction_status: {}\noutput_id: {}",
                evidence.approval_id.as_deref().unwrap_or("<none>"),
                evidence.selected_execution_path(),
                evidence.path_selection_reason(),
                evidence.provider_result_delivery_status,
                evidence.recovery_reason.unwrap_or("<none>"),
                evidence.command_block_id,
                evidence.command,
                evidence.cwd,
                evidence.end_cwd,
                crate::evidence::redact_sensitive_text(&handoff_request.exact_preview).0,
                handoff_request.preview_hash,
                handoff_request.actor,
                handoff_request.source,
                handoff_request.request_id.as_deref().unwrap_or("<none>"),
                handoff_request.tool_use_id.as_deref().unwrap_or("<none>"),
                evidence.status,
                evidence.exit_code,
                evidence.duration_ms,
                evidence.redaction_status,
                evidence.terminal_output_ref.as_ref().map_or_else(
                    || "<none>".to_string(),
                    |_| crate::evidence::output_policy::terminal_output_id(
                        &evidence.shell_session_id,
                        &evidence.command_block_id
                    )
                )
            ),
            presentation: None,
        });
        ids.push(id);
    }
    ids
}

fn shell_handoff_block_matches_request(
    block: &CommandBlock,
    request: &ShellHandoffRequest,
) -> bool {
    // An explicit token on the block decides alone (#2142 review R5): two
    // approved handoffs for the identical command can sit in the pending
    // queue, and a text/origin/time fallback would associate the block
    // carrying the *second* request's token with the *first* request,
    // mis-pairing results and leaving the second request hanging.
    if let Some(block_token) = block
        .audit_identity
        .as_ref()
        .and_then(|audit| audit.handoff_token.as_deref())
    {
        return !request.token.is_empty() && block_token == request.token;
    }
    // Text fallback only for blocks produced by marker scripts that predate
    // the token sidecar. Shell markers are second-granular, so compare
    // representable seconds.
    let started_at_or_after_request = block.started_at_ms / 1_000 >= request.created_at_ms / 1_000;
    block.command == request.command
        && block.origin == expected_handoff_origin(request)
        && started_at_or_after_request
}

fn expected_handoff_origin(request: &ShellHandoffRequest) -> CommandOrigin {
    match request.source.as_str() {
        "send_to_shell" => CommandOrigin::UserSendToShell,
        "user_analysis_action" => CommandOrigin::UserAnalysisAction,
        "approved_provider_shell_tool" => CommandOrigin::ProviderTool,
        "approved_fallback" => CommandOrigin::AgentHandoff,
        "validation" => CommandOrigin::ShellInternal,
        _ => CommandOrigin::Unknown,
    }
}

fn next_shell_handoff_activity_id(state: &InlineState, approval_id: &str) -> String {
    if approval_id.starts_with("handoff-")
        && !state.activity.rows.iter().any(|row| row.id == approval_id)
    {
        return approval_id.to_string();
    }

    let reserved_handoff_ids = state.control.interactive_shell_handoff_ids();
    next_activity_id_excluding(state, "handoff", reserved_handoff_ids)
}
