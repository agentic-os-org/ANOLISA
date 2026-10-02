use crate::agent::run::ActiveAgentRun;
use crate::runtime::prelude::*;
use crate::types::PROVIDER_TIMEOUT_ERROR_CODE;

#[cfg(test)]
const SHELL_HANDOFF_CONTINUATION_HINT: &str = crate::types::SHELL_HANDOFF_CONTINUATION_HINT;
const SHELL_HANDOFF_RECOVERY_OWNER_HINT: &str = "shell handoff recovery owner:";
const DISABLE_PROVIDER_RESUME_HINT: &str = "disable provider resume for shell handoff fallback";
// First fallback tier (T2) for provider timeouts: retry within the same
// provider session so the committed history and the provider session uuid
// survive. Must not contain the "disable provider resume"
// substring the adapters gate on.
const SHELL_HANDOFF_SAME_SESSION_RETRY_HINT: &str = "same-session retry for shell handoff fallback";
const SHELL_HANDOFF_FALLBACK_REASON_HINT: &str = "shell handoff fallback reason:";
const RESUME_FAILED_REASON: &str = "resume_failed";
const PROVIDER_TIMEOUT_REASON: &str = "provider_timeout";

pub(crate) fn run_request_is_analysis_only_continuation(
    run_request: Option<&AgentRequest>,
) -> bool {
    run_request.is_some_and(crate::types::request_is_analysis_only_continuation)
}

pub(crate) fn provider_mode_for_agent_run(
    request: &AgentRequest,
    shell_mode: CoshApprovalMode,
) -> CoshApprovalMode {
    if run_request_is_analysis_only_continuation(Some(request)) {
        CoshApprovalMode::Recommend
    } else {
        shell_mode
    }
}

pub(crate) fn annotate_continuation_user_approval_mode(
    request: &mut AgentRequest,
    shell_mode: CoshApprovalMode,
) {
    if !run_request_is_analysis_only_continuation(Some(request)) {
        return;
    }
    let mode_name = match shell_mode {
        CoshApprovalMode::Recommend => "recommend",
        CoshApprovalMode::Auto => "auto",
        CoshApprovalMode::Trust => "trust",
    };
    request
        .context_hints
        .retain(|hint| !hint.starts_with(crate::types::USER_APPROVAL_MODE_HINT_PREFIX));
    request.context_hints.push(format!(
        "{}{mode_name}",
        crate::types::USER_APPROVAL_MODE_HINT_PREFIX
    ));
}

fn run_request_is_shell_handoff_recovery_continuation(request: &AgentRequest) -> bool {
    run_request_is_analysis_only_continuation(Some(request))
        && request
            .context_hints
            .iter()
            .any(|hint| hint.contains(SHELL_HANDOFF_RECOVERY_OWNER_HINT))
}

pub(crate) fn shell_handoff_recovery_approval_id(request: &AgentRequest) -> Option<&str> {
    request.context_hints.iter().find_map(|hint| {
        hint.strip_prefix(SHELL_HANDOFF_RECOVERY_OWNER_HINT)
            .map(str::trim)
            .and_then(|owner| owner.split('/').next())
            .filter(|approval_id| !approval_id.is_empty())
    })
}

pub(crate) fn render_fresh_turn_recovery_notice<W: Write>(
    state: &InlineState,
    output: &mut W,
    reason: &str,
) -> std::io::Result<()> {
    let reason_line = state
        .i18n()
        .format(MessageId::AgentRecoveryTriggerLine, &[("reason", reason)]);
    RatatuiInlineRenderer::for_terminal()
        .with_language(state.language)
        .write_notice_panel(
            output,
            NoticePanelModel {
                title: state.i18n().t(MessageId::AgentRecoveryTitle),
                body: vec![
                    state
                        .i18n()
                        .t(MessageId::AgentRecoveryFreshTurnBody)
                        .to_string(),
                    state
                        .i18n()
                        .t(MessageId::AgentRecoveryContinuityBody)
                        .to_string(),
                    reason_line,
                ],
                footer: None,
            },
        )
}

/// A same-session retry (T2) keeps provider continuity, so the fresh-turn
/// recovery panel copy would be wrong for it; emit the trigger reason plus
/// one lightweight retry line. The reason must render here too because a
/// successful T2 never reaches the T3 panel.
pub(crate) fn render_same_session_retry_notice<W: Write>(
    state: &InlineState,
    reason: &str,
    output: &mut W,
) -> std::io::Result<()> {
    writeln!(
        output,
        "{}",
        state
            .i18n()
            .format(MessageId::AgentRecoveryTriggerLine, &[("reason", reason)])
    )?;
    writeln!(
        output,
        "{}",
        state.i18n().t(MessageId::AgentRecoverySameSessionRetryLine)
    )
}

/// Renders the recovery notice matching the fallback tier: the fresh-turn
/// panel only for a true fresh turn (T3), the trigger-reason and retry
/// status lines for the same-session retry (T2).
pub(crate) fn render_fallback_recovery_notice<W: Write>(
    state: &InlineState,
    fallback: &AgentRequest,
    reason: &str,
    output: &mut W,
) -> std::io::Result<()> {
    if fallback_request_is_fresh_turn(fallback) {
        render_fresh_turn_recovery_notice(state, output, reason)
    } else {
        render_same_session_retry_notice(state, reason, output)
    }
}

pub(crate) fn fallback_request_is_fresh_turn(request: &AgentRequest) -> bool {
    request
        .context_hints
        .iter()
        .any(|hint| hint.contains(DISABLE_PROVIDER_RESUME_HINT))
}

pub(crate) fn shell_handoff_resume_fallback_request(
    active_run: &ActiveAgentRun,
) -> Option<(AgentRequest, AgentRunOrigin, &'static str)> {
    let reason = active_run.governed_events.iter().rev().find_map(|event| {
        let AgentEvent::AgentFailed { error_code, .. } = &event.event else {
            return None;
        };
        Some(
            if error_code.as_deref() == Some(PROVIDER_TIMEOUT_ERROR_CODE) {
                PROVIDER_TIMEOUT_REASON
            } else {
                RESUME_FAILED_REASON
            },
        )
    });

    shell_handoff_resume_fallback_request_for_reason(active_run, reason?)
}

fn shell_handoff_resume_fallback_request_for_reason(
    active_run: &ActiveAgentRun,
    reason: &'static str,
) -> Option<(AgentRequest, AgentRunOrigin, &'static str)> {
    if !run_request_is_shell_handoff_recovery_continuation(&active_run.request) {
        return None;
    }
    // Tier chain: T1 continuation -> T2 same-session retry ->
    // T3 fresh safety net -> stop. Hints only ever accumulate, so the chain
    // is monotonic and capped at three turns. A rejected resume
    // (resume_failed) skips T2: retrying the same session would fail the
    // same way, so it goes straight to the fresh safety net.
    if active_run
        .request
        .context_hints
        .iter()
        .any(|hint| hint.contains(DISABLE_PROVIDER_RESUME_HINT))
    {
        return None;
    }
    let retried_same_session = active_run
        .request
        .context_hints
        .iter()
        .any(|hint| hint.contains(SHELL_HANDOFF_SAME_SESSION_RETRY_HINT));

    let mut request = active_run.request.clone();
    if reason == PROVIDER_TIMEOUT_REASON && !retried_same_session {
        // T2: retry within the same provider session, keeping the committed
        // history and the provider session uuid.
        request.id = format!("{}-retry", request.id);
        request.command_block.id = format!("{}-retry", request.command_block.id);
        request
            .context_hints
            .push(SHELL_HANDOFF_SAME_SESSION_RETRY_HINT.to_string());
    } else {
        // T3: the same-session retry stalled too (or the resume itself was
        // rejected); fall back to a fresh provider turn so the user still
        // gets an answer.
        request.id = format!("{}-fresh", request.id);
        request.command_block.id = format!("{}-fresh", request.command_block.id);
        request
            .context_hints
            .push(DISABLE_PROVIDER_RESUME_HINT.to_string());
    }
    request
        .context_hints
        .push(format!("{SHELL_HANDOFF_FALLBACK_REASON_HINT} {reason}"));
    Some((request, active_run.origin, reason))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn analysis_only_continuation_guard_requires_mode_and_hint() {
        let mut request = test_request();
        request.mode = AgentMode::RecommendOnly;
        request.context_hints = vec![SHELL_HANDOFF_CONTINUATION_HINT.to_string()];
        assert!(run_request_is_analysis_only_continuation(Some(&request)));

        request.context_hints.clear();
        assert!(!run_request_is_analysis_only_continuation(Some(&request)));

        request.mode = AgentMode::AnalysisOnly;
        request.context_hints = vec![SHELL_HANDOFF_CONTINUATION_HINT.to_string()];
        assert!(!run_request_is_analysis_only_continuation(Some(&request)));
        assert!(!run_request_is_analysis_only_continuation(None));
    }

    #[test]
    fn shell_handoff_resume_fallback_retries_once_without_resume() {
        let mut request = test_request();
        request.context_hints = vec![
            SHELL_HANDOFF_CONTINUATION_HINT.to_string(),
            format!("{SHELL_HANDOFF_RECOVERY_OWNER_HINT} req-1/toolu-1"),
        ];
        let mut active_run = test_active_run(request);
        active_run.origin = AgentRunOrigin::InsightPrompt;
        active_run.governed_events.push(GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::AuditOnly,
            event: AgentEvent::AgentFailed {
                run_id: "run-1".to_string(),
                error: "provider resume session was rejected".to_string(),
                error_code: None,
                max_turns: None,
            },
            reason: "failed".to_string(),
            display_text: "failed".to_string(),
            auto_execute: false,
        });

        let (fallback, origin, reason) =
            shell_handoff_resume_fallback_request(&active_run).expect("fallback request");
        assert_eq!(origin, AgentRunOrigin::InsightPrompt);
        assert_eq!(reason, RESUME_FAILED_REASON);
        assert_eq!(fallback.id, "request-1-fresh");
        assert_eq!(fallback.command_block.id, "block-1-fresh");
        assert!(fallback
            .context_hints
            .iter()
            .any(|hint| hint.contains(DISABLE_PROVIDER_RESUME_HINT)));
        assert!(fallback
            .context_hints
            .iter()
            .any(|hint| hint == "shell handoff fallback reason: resume_failed"));

        let mut retry = test_active_run(fallback);
        retry.governed_events.push(GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::AuditOnly,
            event: AgentEvent::AgentFailed {
                run_id: "run-2".to_string(),
                error: "fresh provider turn failed".to_string(),
                error_code: None,
                max_turns: None,
            },
            reason: "failed".to_string(),
            display_text: "failed".to_string(),
            auto_execute: false,
        });
        assert!(shell_handoff_resume_fallback_request(&retry).is_none());
    }

    #[test]
    fn shell_handoff_active_continuation_age_does_not_trigger_fallback() {
        let mut request = test_request();
        request.context_hints = vec![
            SHELL_HANDOFF_CONTINUATION_HINT.to_string(),
            format!("{SHELL_HANDOFF_RECOVERY_OWNER_HINT} req-1/toolu-1"),
        ];
        let mut active_run = test_active_run(request);
        active_run.started_at = Instant::now() - std::time::Duration::from_secs(60);
        active_run.last_activity_at = Instant::now();
        assert!(shell_handoff_resume_fallback_request(&active_run).is_none());
    }

    #[test]
    fn shell_handoff_cancel_does_not_start_fresh_fallback() {
        let mut request = test_request();
        request.context_hints = vec![
            SHELL_HANDOFF_CONTINUATION_HINT.to_string(),
            format!("{SHELL_HANDOFF_RECOVERY_OWNER_HINT} req-1/toolu-1"),
        ];
        let mut active_run = test_active_run(request);
        active_run.governed_events.push(GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::AuditOnly,
            event: AgentEvent::AgentCancelled {
                run_id: "run-1".to_string(),
                reason: "user requested cancellation".to_string(),
            },
            reason: "cancelled".to_string(),
            display_text: "cancelled".to_string(),
            auto_execute: false,
        });

        assert!(shell_handoff_resume_fallback_request(&active_run).is_none());
    }

    #[test]
    fn shell_handoff_provider_timeout_records_trigger_reason() {
        let mut request = test_request();
        request.context_hints = vec![
            SHELL_HANDOFF_CONTINUATION_HINT.to_string(),
            format!("{SHELL_HANDOFF_RECOVERY_OWNER_HINT} req-1/toolu-1"),
        ];
        let mut active_run = test_active_run(request);
        active_run.origin = AgentRunOrigin::AutoFailure;
        active_run.governed_events.push(GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::AuditOnly,
            event: AgentEvent::AgentFailed {
                run_id: "run-1".to_string(),
                error: "watchdog expired".to_string(),
                error_code: Some(PROVIDER_TIMEOUT_ERROR_CODE.to_string()),
                max_turns: None,
            },
            reason: "failed".to_string(),
            display_text: "failed".to_string(),
            auto_execute: false,
        });

        let (fallback, origin, reason) =
            shell_handoff_resume_fallback_request(&active_run).expect("fallback request");
        assert_eq!(origin, AgentRunOrigin::AutoFailure);
        assert_eq!(reason, PROVIDER_TIMEOUT_REASON);
        assert!(fallback
            .context_hints
            .iter()
            .any(|hint| hint == "shell handoff fallback reason: provider_timeout"));
    }

    // A provider timeout escalates through a same-session retry
    // (T2, resume stays enabled) before the fresh safety net (T3) disables
    // resume; the chain stops after T3. A rejected resume (resume_failed,
    // covered above) skips T2 and goes straight to T3.
    #[test]
    fn shell_handoff_provider_timeout_escalates_through_retry_then_fresh() {
        fn timeout_event(run_id: &str) -> GovernedEvent {
            GovernedEvent {
                decision: GovernanceDecision::Display,
                policy_decision: GovernancePolicyDecision::AuditOnly,
                event: AgentEvent::AgentFailed {
                    run_id: run_id.to_string(),
                    error: "watchdog expired".to_string(),
                    error_code: Some(PROVIDER_TIMEOUT_ERROR_CODE.to_string()),
                    max_turns: None,
                },
                reason: "failed".to_string(),
                display_text: "failed".to_string(),
                auto_execute: false,
            }
        }

        let mut request = test_request();
        request.context_hints = vec![
            SHELL_HANDOFF_CONTINUATION_HINT.to_string(),
            format!("{SHELL_HANDOFF_RECOVERY_OWNER_HINT} req-1/toolu-1"),
        ];
        let mut active_run = test_active_run(request);
        active_run.governed_events.push(timeout_event("run-1"));

        // T2: same-session retry keeps provider resume enabled.
        let (retry, _origin, reason) =
            shell_handoff_resume_fallback_request(&active_run).expect("retry request");
        assert_eq!(reason, PROVIDER_TIMEOUT_REASON);
        assert_eq!(retry.id, "request-1-retry");
        assert_eq!(retry.command_block.id, "block-1-retry");
        assert!(!fallback_request_is_fresh_turn(&retry));
        assert!(retry
            .context_hints
            .iter()
            .any(|hint| hint.contains(SHELL_HANDOFF_SAME_SESSION_RETRY_HINT)));

        // T3: the retry timed out too; the fresh safety net disables resume.
        let mut retry_run = test_active_run(retry);
        retry_run.governed_events.push(timeout_event("run-2"));
        let (fresh, _origin, _reason) =
            shell_handoff_resume_fallback_request(&retry_run).expect("fresh request");
        assert_eq!(fresh.id, "request-1-retry-fresh");
        assert!(fallback_request_is_fresh_turn(&fresh));

        // Chain cap: no fallback beyond the fresh safety net.
        let mut exhausted = test_active_run(fresh);
        exhausted.governed_events.push(timeout_event("run-3"));
        assert!(shell_handoff_resume_fallback_request(&exhausted).is_none());
    }

    // The adapters (cosh_core.rs, qwen.rs, claude.rs) gate provider resume on
    // the literal substring "disable provider resume" rather than on these
    // constants. Anchor the cross-module contract so renaming either hint
    // cannot silently flip the T2/T3 resume behavior.
    #[test]
    fn fallback_hints_keep_the_adapter_resume_gating_contract() {
        const ADAPTER_RESUME_GATE_SUBSTRING: &str = "disable provider resume";
        assert!(DISABLE_PROVIDER_RESUME_HINT.contains(ADAPTER_RESUME_GATE_SUBSTRING));
        assert!(!SHELL_HANDOFF_SAME_SESSION_RETRY_HINT.contains(ADAPTER_RESUME_GATE_SUBSTRING));

        let mut request = test_request();
        request.context_hints = vec![SHELL_HANDOFF_SAME_SESSION_RETRY_HINT.to_string()];
        assert!(!fallback_request_is_fresh_turn(&request));
        request
            .context_hints
            .push(DISABLE_PROVIDER_RESUME_HINT.to_string());
        assert!(fallback_request_is_fresh_turn(&request));
    }

    #[test]
    fn shell_handoff_continuation_uses_recommend_provider_mode() {
        let mut request = test_request();
        request.context_hints = vec![SHELL_HANDOFF_CONTINUATION_HINT.to_string()];

        assert_eq!(
            provider_mode_for_agent_run(&request, CoshApprovalMode::Auto),
            CoshApprovalMode::Recommend
        );

        request.context_hints.clear();
        assert_eq!(
            provider_mode_for_agent_run(&request, CoshApprovalMode::Auto),
            CoshApprovalMode::Auto
        );
    }

    fn test_active_run(request: AgentRequest) -> ActiveAgentRun {
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

    fn test_request() -> AgentRequest {
        AgentRequest {
            id: "request-1".to_string(),
            session_id: "session-1".to_string(),
            command_block: CommandBlock {
                id: "block-1".to_string(),
                session_id: "session-1".to_string(),
                command: "continuation".to_string(),
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
            user_input: Some("continuation".to_string()),
            findings: Vec::new(),
            mode: AgentMode::RecommendOnly,
            user_confirmed: true,
            hook_finding: None,
            recommended_skill: None,
        }
    }
}
