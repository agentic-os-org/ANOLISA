pub(crate) use super::structured_events::{
    render_agent_structured_events, render_new_agent_structured_events,
};
use crate::agent::heartbeat::{remember_agent_activity, render_agent_pending_tool_status};
use crate::agent::run::ActiveAgentRun;
use crate::runtime::prelude::*;

mod classification;

use classification::{
    event_updates_pending_tool_status, is_interaction_governed_event,
    should_render_governance_block,
};

pub(super) fn event_may_render_structured_surface(event: &GovernedEvent) -> bool {
    classification::event_may_render_structured_surface(event)
}

pub(crate) fn render_active_agent_event<W: Write>(
    active_run: &mut ActiveAgentRun,
    event: AgentEvent,
    output: &mut W,
    text_hold_reason: Option<TextHoldReason>,
) -> std::io::Result<()> {
    if let AgentEvent::HookNotification {
        ref tool_use_id,
        ref hook_name,
        ref message,
        ref decision,
        ..
    } = event
    {
        if tool_use_id.is_some() {
            active_run.pending_hook_notifications.push(
                crate::agent::run::PendingHookNotification {
                    tool_use_id: tool_use_id.clone(),
                    hook_name: hook_name.clone(),
                    message: message.clone(),
                    decision: decision.clone(),
                },
            );
            // Still record in governed_events for audit, but don't defer for rendering
            let governed = govern_agent_events_with_language(
                &[event],
                &Policy::default(),
                active_run.language,
            )
            .events;
            active_run.governed_events.extend(governed);
            return Ok(());
        }
    }

    let mut governed =
        govern_agent_events_with_language(&[event], &Policy::default(), active_run.language).events;
    filter_cosh_request_text_deltas(active_run, &mut governed);
    remember_agent_activity(active_run, &governed);
    let should_render_pending_tool_status = governed.iter().any(event_updates_pending_tool_status);
    if governed
        .first()
        .is_some_and(|event| matches!(event.event, AgentEvent::TextDelta { .. }))
    {
        if text_hold_reason.is_some() {
            active_run.held_events.extend(governed.clone());
        } else {
            active_run.status_animation.clear(output)?;
            render_held_events_into_active_run(active_run, &governed, output)?;
        }
    } else if governed
        .first()
        .is_some_and(|event| matches!(event.event, AgentEvent::AgentCompleted { .. }))
    {
    } else {
        active_run.deferred_events.extend(
            governed
                .iter()
                .filter(|event| should_render_governance_block(event))
                .cloned(),
        );
    }
    active_run.governed_events.extend(governed);
    if should_render_pending_tool_status {
        render_agent_pending_tool_status(active_run, output)?;
    }
    Ok(())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextHoldReason {
    InteractionPending,
    QueuedBeforeHeldText,
    UnrenderedInteraction,
    PostToolShellTranscript,
    PostToolShellResult,
    ForcedDeferredPoll,
}

pub(crate) fn state_has_pending_interaction(state: &InlineState) -> bool {
    has_pending_question(state)
        || crate::auth::runtime::has_pending_auth(state)
        || state
            .approvals
            .requests
            .iter()
            .any(|request| request.status == ApprovalRequestStatus::Pending)
}

pub(crate) fn active_run_has_unrendered_interaction(active_run: &ActiveAgentRun) -> bool {
    active_run.governed_events[active_run.rendered_governed_event_count..]
        .iter()
        .any(is_interaction_governed_event)
}

pub(crate) fn render_held_events_into_active_run<W: Write>(
    active_run: &mut ActiveAgentRun,
    events: &[GovernedEvent],
    output: &mut W,
) -> std::io::Result<()> {
    for event in events {
        if matches!(event.event, AgentEvent::TextDelta { .. }) {
            if event.display_text.is_empty() {
                continue;
            }
            active_run
                .markdown_stream
                .write_delta(output, &event.display_text)?;
            active_run.has_visible_text_delta = true;
        } else if should_render_governance_block(event) {
            active_run
                .renderer
                .write_governed_events(output, std::slice::from_ref(event))?;
        }
    }
    Ok(())
}

pub(crate) fn flush_cosh_request_filter_into_active_run<W: Write>(
    active_run: &mut ActiveAgentRun,
    output: &mut W,
) -> std::io::Result<()> {
    let filtered = active_run.cosh_request_filter.finish();
    active_run.pending_cosh_requests.extend(filtered.requests);
    active_run
        .pending_cosh_request_audits
        .extend(filtered.audit_records);
    if filtered.visible_text.is_empty() {
        return Ok(());
    }
    active_run
        .markdown_stream
        .write_delta(output, &filtered.visible_text)?;
    active_run.has_visible_text_delta = true;
    active_run.governed_events.push(GovernedEvent {
        decision: GovernanceDecision::Display,
        policy_decision: GovernancePolicyDecision::DisplayOnly,
        event: AgentEvent::TextDelta {
            run_id: active_run.request.id.clone(),
            text: filtered.visible_text.clone(),
        },
        reason: "released incomplete cosh-request stream buffer".to_string(),
        display_text: filtered.visible_text,
        auto_execute: false,
    });
    Ok(())
}

fn filter_cosh_request_text_deltas(
    active_run: &mut ActiveAgentRun,
    governed_events: &mut Vec<GovernedEvent>,
) {
    for event in governed_events {
        let AgentEvent::TextDelta { text, .. } = &mut event.event else {
            continue;
        };
        let filtered = active_run.cosh_request_filter.filter_delta(text);
        active_run.pending_cosh_requests.extend(filtered.requests);
        active_run
            .pending_cosh_request_audits
            .extend(filtered.audit_records);
        *text = filtered.visible_text.clone();
        event.display_text = filtered.visible_text;
    }
}

pub(crate) fn flush_held_agent_events<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state_has_pending_interaction(state) {
        return Ok(());
    }
    if state.control.shell_handoff().has_active_handoff() {
        return Ok(());
    }

    if let Some(active_run) = state.agent_run.active.as_mut() {
        if active_run.held_events.is_empty() {
            return Ok(());
        }
        active_run.status_animation.clear(output)?;
        let held_events = std::mem::take(&mut active_run.held_events);
        render_held_events_into_active_run(active_run, &held_events, output)?;
        output.flush()?;
        return Ok(());
    }

    if state.agent_run.held_events.is_empty() {
        return Ok(());
    }
    let held_events = std::mem::take(&mut state.agent_run.held_events);
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let mut stream = renderer.stream_markdown_agent();
    for event in &held_events {
        if matches!(event.event, AgentEvent::TextDelta { .. }) {
            stream.write_delta(output, &event.display_text)?;
        } else if should_render_governance_block(event) {
            renderer.write_governed_events(output, std::slice::from_ref(event))?;
        }
    }
    stream.finish(output, None)?;
    output.flush()?;
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn status_changed_does_not_request_structured_surface_clear() {
        let governed = GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::StatusChanged {
                run_id: "run-1".to_string(),
                phase: "thinking".to_string(),
                message: "thinking".to_string(),
            },
            reason: "status".to_string(),
            display_text: String::new(),
            auto_execute: false,
        };
        assert!(!event_may_render_structured_surface(&governed));
    }

    #[test]
    fn ordinary_tool_call_does_not_request_structured_surface_clear() {
        let governed = GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("read-1".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            },
            reason: "tool call".to_string(),
            display_text: String::new(),
            auto_execute: false,
        };
        assert!(!event_may_render_structured_surface(&governed));
    }

    #[test]
    fn tool_output_delta_does_not_request_structured_surface_clear() {
        let governed = GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::ToolOutputDelta {
                run_id: "run-1".to_string(),
                tool_id: "read-1".to_string(),
                stream: "stdout".to_string(),
                text: "output".to_string(),
            },
            reason: "tool output".to_string(),
            display_text: String::new(),
            auto_execute: false,
        };
        assert!(!event_may_render_structured_surface(&governed));
    }

    #[test]
    fn tool_completed_requests_structured_surface_clear() {
        let governed = GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::ToolCompleted {
                run_id: "run-1".to_string(),
                tool_id: "grep".to_string(),
                status: "success".to_string(),
            },
            reason: "tool done".to_string(),
            display_text: String::new(),
            auto_execute: false,
        };
        assert!(event_may_render_structured_surface(&governed));
    }

    #[test]
    fn shell_evidence_request_requests_structured_surface_clear() {
        let governed = GovernedEvent {
            decision: GovernanceDecision::Display,
            policy_decision: GovernancePolicyDecision::DisplayOnly,
            event: AgentEvent::ShellEvidenceRequest {
                run_id: "run-1".to_string(),
                request_id: "evidence-1".to_string(),
                tool_use_id: "toolu-evidence".to_string(),
                action: crate::adapter::ShellEvidenceAction::ListCommands {
                    limit: 2,
                    cursor: None,
                },
            },
            reason: "shell evidence".to_string(),
            display_text: String::new(),
            auto_execute: false,
        };
        assert!(event_may_render_structured_surface(&governed));
    }

    #[test]
    fn active_agent_text_delta_suppresses_cosh_request_block() {
        let mut active_run = test_active_run();
        let mut output = Vec::new();
        render_active_agent_event(
            &mut active_run,
            AgentEvent::TextDelta {
                run_id: "run-1".to_string(),
                text: "before\n```cosh-request\nhistory\n```\nafter".to_string(),
            },
            &mut output,
            None,
        )
        .expect("render event");

        assert_eq!(active_run.pending_cosh_requests.len(), 1);
        assert!(active_run
            .governed_events
            .iter()
            .all(|event| !event.display_text.contains("cosh-request")));
        assert!(active_run
            .governed_events
            .iter()
            .all(|event| !matches!(&event.event, AgentEvent::TextDelta { text, .. } if text.contains("cosh-request"))));
    }

    #[test]
    fn text_delta_without_hold_reason_bypasses_hold_queue() {
        let mut active_run = test_active_run();
        let mut output = Vec::new();

        render_active_agent_event(
            &mut active_run,
            AgentEvent::TextDelta {
                run_id: "run-1".to_string(),
                text: "TEXT ONLY STREAMS".to_string(),
            },
            &mut output,
            None,
        )
        .expect("render event");

        assert!(active_run.held_events.is_empty());
        assert!(active_run.has_visible_text_delta);
    }

    #[test]
    fn text_delta_with_post_tool_hold_reason_is_held() {
        let mut active_run = test_active_run();
        let mut output = Vec::new();

        render_active_agent_event(
            &mut active_run,
            AgentEvent::TextDelta {
                run_id: "run-1".to_string(),
                text: "POST TOOL TEXT WAITS".to_string(),
            },
            &mut output,
            Some(TextHoldReason::PostToolShellResult),
        )
        .expect("render event");

        let output = String::from_utf8(output).expect("utf8");
        assert!(!output.contains("POST TOOL TEXT WAITS"), "{output}");
        assert_eq!(active_run.held_events.len(), 1);
        assert!(!active_run.has_visible_text_delta);
    }

    #[test]
    fn tool_call_after_text_finishes_agent_block_and_renders_status() {
        let mut active_run = test_active_run();
        let mut output = Vec::new();

        render_active_agent_event(
            &mut active_run,
            AgentEvent::TextDelta {
                run_id: "run-1".to_string(),
                text: "Before tool call.\n\n".to_string(),
            },
            &mut output,
            None,
        )
        .expect("render text");
        render_active_agent_event(
            &mut active_run,
            AgentEvent::ToolCall {
                run_id: "run-1".to_string(),
                tool_id: Some("read-1".to_string()),
                name: "Read".to_string(),
                input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
            },
            &mut output,
            None,
        )
        .expect("render tool call");

        let output = String::from_utf8(output).expect("utf8");
        assert!(output.contains("Before tool call."), "{output}");
        assert!(!active_run.markdown_stream.has_started());
        assert!(output.contains("Thinking... reading file: 1"), "{output}");
        assert!(
            active_run.current_message.contains("reading file: 1"),
            "{}",
            active_run.current_message
        );
    }

    #[test]
    fn tool_result_between_text_deltas_splits_agent_cards() {
        let mut state = InlineState::default();
        let adapter = AdapterInstance::Fake(FakeAgentAdapter);
        let mut active_run = test_active_run();
        let renderer = RatatuiInlineRenderer::with_width(100);
        active_run.renderer = renderer.clone();
        active_run.status_animation = renderer.status_animation();
        active_run.markdown_stream = renderer.stream_markdown_agent();
        state.agent_run.active = Some(active_run);
        let mut output = Vec::new();

        {
            let active_run = state.agent_run.active.as_mut().expect("active run");
            render_active_agent_event(
                active_run,
                AgentEvent::TextDelta {
                    run_id: "run-1".to_string(),
                    text: "Before tool result.".to_string(),
                },
                &mut output,
                None,
            )
            .expect("render text");
            render_active_agent_event(
                active_run,
                AgentEvent::ToolCall {
                    run_id: "run-1".to_string(),
                    tool_id: Some("read-1".to_string()),
                    name: "Read".to_string(),
                    input: r#"{"file_path":"Cargo.toml"}"#.to_string(),
                },
                &mut output,
                None,
            )
            .expect("render tool call");
            render_active_agent_event(
                active_run,
                AgentEvent::ToolCompleted {
                    run_id: "run-1".to_string(),
                    tool_id: "read-1".to_string(),
                    status: "success".to_string(),
                },
                &mut output,
                None,
            )
            .expect("render tool result");
        }

        render_new_agent_structured_events(&mut state, &mut output, &adapter)
            .expect("render structured events");
        assert!(
            !state
                .agent_run
                .active
                .as_ref()
                .expect("active run")
                .has_visible_text_delta
        );

        {
            let active_run = state.agent_run.active.as_mut().expect("active run");
            render_active_agent_event(
                active_run,
                AgentEvent::TextDelta {
                    run_id: "run-1".to_string(),
                    text: "After tool result.\n\n".to_string(),
                },
                &mut output,
                None,
            )
            .expect("render text after tool");
        }

        let output = String::from_utf8(output).expect("utf8");
        let before = output.find("Before tool result.").expect("before text");
        let first_bottom = output[before..].find('╰').expect("first card bottom") + before;
        let tool_card = output[first_bottom..]
            .find("Read completed")
            .expect("tool card")
            + first_bottom;
        let after_title = output[tool_card..]
            .find("╭ Agent")
            .expect("second agent card")
            + tool_card;
        let after_text = output[after_title..]
            .find("After tool result.")
            .expect("after text")
            + after_title;

        assert!(before < first_bottom, "{output}");
        assert!(first_bottom < tool_card, "{output}");
        assert!(tool_card < after_title, "{output}");
        assert!(after_title < after_text, "{output}");
    }

    pub(crate) fn test_active_run() -> ActiveAgentRun {
        let request = AgentRequest {
            id: "request-1".to_string(),
            session_id: "session-1".to_string(),
            command_block: CommandBlock {
                id: "cmd-1".to_string(),
                session_id: "session-1".to_string(),
                command: "hello".to_string(),
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
}
