use std::sync::{mpsc, Arc, Mutex};
use std::time::Instant;

use super::process::{
    parse_compactor_output, ActiveCompaction, TerminationReason, MAX_REPORTED_ERROR_CHARS,
    TERMINATION_GRACE,
};
use super::*;
use crate::adapter::{AdapterInstance, CoshCoreAdapter, FakeAgentAdapter};
use crate::agent::intercept::render_intercept_agent_guidance;
use crate::runtime::state::InlineState;
use crate::types::{ShellEvent, ShellEventKind};

// These are pure state-machine tests: no test here may spawn a subprocess
// (enforced by scripts/check-layout.sh). Real SIGTERM/SIGKILL delivery,
// process-group reaping, and shell-exit cleanup are covered against a real
// compactor child in `tests/raw_cli/compaction.rs`.

/// Process-free active compaction: the state machine is driven entirely
/// through the receiver, the deadline, and the termination fields, with no
/// child to signal (`terminate_and_reap` is a no-op on `None`).
fn childless_active(receiver: mpsc::Receiver<CompactionOutcome>) -> ActiveCompaction {
    let now = Instant::now();
    ActiveCompaction {
        session_id: "00000000-0000-4000-8000-000000000000".to_string(),
        workspace_scope: "/tmp".to_string(),
        started_at: now,
        deadline: now + super::process::COMPACTOR_DEADLINE,
        origin: CompactionOrigin::Manual,
        revision_marker: None,
        termination: None,
        stderr_tail: crate::adapter::StderrTail::new(1024),
        child: Arc::new(Mutex::new(None)),
        receiver,
    }
}

fn state_with_childless_compaction() -> (InlineState, mpsc::Sender<CompactionOutcome>) {
    let (sender, receiver) = mpsc::channel();
    let mut state = InlineState::default();
    state.control.session_mut().compaction_mut().active = Some(childless_active(receiver));
    (state, sender)
}

fn natural_language_event(input: &str) -> ShellEvent {
    let mut event = ShellEvent::user_input_intercepted("shell-session", input);
    event.kind = ShellEventKind::UserInputIntercepted;
    event.component = Some("natural_language".to_string());
    event
}

fn path_prompt_event(input: &str) -> ShellEvent {
    let mut event = natural_language_event(input);
    event.component = Some("path_prompt".to_string());
    event.cwd = Some("/repo".to_string());
    event
}

#[test]
fn compact_requires_cosh_core_backend() {
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut state = InlineState::default();
    let mut output = Vec::new();

    render_session_compact_command(None, &[], &adapter, &mut state, &mut output)
        .expect("render unavailable");

    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("cosh-core"), "{rendered}");
    assert!(!compaction_active(&state));
}

#[test]
fn compact_without_active_session_is_rejected() {
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let mut state = InlineState::default();
    let mut output = Vec::new();

    render_session_compact_command(None, &[], &adapter, &mut state, &mut output)
        .expect("render no-session notice");

    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(
        rendered.contains("No active resumable cosh-core session"),
        "{rendered}"
    );
    assert!(!compaction_active(&state));
}

#[test]
fn duplicate_compact_is_rejected_while_running() {
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let (mut state, _sender) = state_with_childless_compaction();
    let mut output = Vec::new();

    render_session_compact_command(None, &[], &adapter, &mut state, &mut output)
        .expect("render duplicate notice");

    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("already running"), "{rendered}");
}

#[test]
fn conflicting_session_mutations_are_rejected_during_compaction() {
    let (state, _sender) = state_with_childless_compaction();
    assert!(!super::super::panel::session_management_idle(&state));
}

#[test]
fn status_reports_running_and_idle_states() {
    let (state, _sender) = state_with_childless_compaction();
    let mut output = Vec::new();
    render_compaction_status(&state, &mut output).expect("render status");
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("running"), "{rendered}");
    assert!(
        rendered.contains("00000000-0000-4000-8000-000000000000"),
        "{rendered}"
    );

    let idle = InlineState::default();
    let mut output = Vec::new();
    render_compaction_status(&idle, &mut output).expect("render idle status");
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("No background compaction"), "{rendered}");
}

#[test]
fn cancel_requests_termination_of_the_active_compactor() {
    let (mut state, _sender) = state_with_childless_compaction();
    let mut output = Vec::new();

    cancel_compaction(&mut state, &mut output).expect("render cancel notice");

    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("Cancellation requested"), "{rendered}");
    // The user cancel arms the termination state machine; actual SIGTERM
    // delivery, SIGKILL escalation, and process-group reaping against a real
    // child are verified in `tests/raw_cli/compaction.rs`.
    assert!(state
        .control
        .session()
        .compaction()
        .active
        .as_ref()
        .expect("still active during grace")
        .cancel_requested());

    let mut idle = InlineState::default();
    let mut output = Vec::new();
    cancel_compaction(&mut idle, &mut output).expect("render not-running notice");
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("No background compaction"), "{rendered}");
}

#[test]
fn completion_is_deferred_while_foreground_command_is_active() {
    let (mut state, sender) = state_with_childless_compaction();
    sender
        .send(CompactionOutcome::Committed {
            tokens_before: 74_210,
            tokens_after: 29_800,
            after_source: "estimated".to_string(),
        })
        .expect("queue outcome");

    // A background compactor completion never spawns an Agent run, so a Fake
    // adapter that would panic if started is a safe stand-in here.
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);

    // Busy foreground: harvest silently, never write through command output.
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, true).expect("poll while busy");
    assert!(output.is_empty());
    assert!(!compaction_active(&state));

    // Next safe prompt boundary: completion renders and prompt is restored.
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll at boundary");
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("74210"), "{rendered}");
    assert!(rendered.contains("29800"), "{rendered}");
    assert!(rendered.contains("estimated"), "{rendered}");

    // The queued completion is consumed exactly once.
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll again");
    assert!(output.is_empty());
}

#[test]
fn cancelled_completion_reports_unchanged_projection() {
    let (mut state, sender) = state_with_childless_compaction();
    state
        .control
        .session_mut()
        .compaction_mut()
        .active
        .as_mut()
        .expect("active")
        .request_termination(TerminationReason::UserCancel);
    sender
        .send(CompactionOutcome::Failed {
            code: "transport".to_string(),
            message: "terminated".to_string(),
        })
        .expect("queue outcome");

    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("cancelled"), "{rendered}");
    // The safe wording must not claim the projection is unchanged.
    assert!(rendered.contains("transcript is unchanged"), "{rendered}");
}

#[test]
fn disconnected_reader_becomes_a_transport_failure() {
    // If the reader thread vanishes without sending a result, the channel
    // disconnects; poll must turn that into a typed transport failure and
    // stop reporting the compaction active — never hang.
    let (mut state, sender) = state_with_childless_compaction();
    drop(sender);

    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("transport"), "{rendered}");
    assert!(!compaction_active(&state));
}

/// Internal, non-intercept start paths (auto failure analysis, hooks,
/// evidence continuations, recovery fallbacks) all funnel through
/// `start_agent_run_with_origin`. The central compaction gate must reject
/// them so no model process is launched against a transcript the background
/// compactor is about to rewrite.
#[test]
fn internal_agent_run_is_suppressed_during_compaction() {
    use crate::agent::run::{start_agent_run_with_origin, AgentRunOrigin};
    use crate::types::{AgentMode, AgentRequest, CommandBlock, CommandStatus, OutputRefs};

    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let (mut state, _sender) = state_with_childless_compaction();
    let request = AgentRequest {
        id: "auto-failure-1".to_string(),
        session_id: "shell-session".to_string(),
        command_block: CommandBlock {
            id: "cmd-1".to_string(),
            session_id: "shell-session".to_string(),
            command: "make".to_string(),
            origin: Default::default(),
            cwd: "/repo".to_string(),
            end_cwd: "/repo".to_string(),
            started_at_ms: 1,
            ended_at_ms: 2,
            duration_ms: 1,
            exit_code: 2,
            status: CommandStatus::Failed,
            output: OutputRefs {
                terminal_output_ref: None,
                terminal_output_bytes: 0,
            },
            shell_environment_generation: None,
            audit_identity: None,
        },
        context_blocks: Vec::new(),
        context_hints: Vec::new(),
        user_input: None,
        findings: Vec::new(),
        mode: AgentMode::AnalysisOnly,
        user_confirmed: false,
        hook_finding: None,
        recommended_skill: None,
    };
    let mut output = Vec::new();

    start_agent_run_with_origin(
        &request,
        AgentRunOrigin::AutoFailure,
        crate::agent::run::AgentStartIntent::InternalBestEffort,
        &adapter,
        &mut state,
        &mut output,
        None,
    )
    .expect("gate returns Ok without starting a run");

    // No model process was launched (the bogus program would have failed if
    // it had been) and nothing was queued behind the paused conversation.
    assert!(state.agent_run.active.is_none());
    assert!(state.agent_run.queued_requests.is_empty());
    assert!(output.is_empty());
}

#[test]
fn suppressed_failed_command_analysis_is_not_marked_analyzed() {
    use crate::agent::failed_command::{
        start_agent_for_block, FailedCommandAgentStartOptions, FailedCommandAnalysisTrigger,
    };
    use crate::runtime::state::AnalysisMode;
    use crate::types::{CommandBlock, CommandStatus, OutputRefs};

    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let (mut state, _sender) = state_with_childless_compaction();
    state.analysis_mode = AnalysisMode::Auto;
    let block = CommandBlock {
        id: "blk-suppressed".to_string(),
        session_id: "shell-session".to_string(),
        command: "make release".to_string(),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 1,
        ended_at_ms: 2,
        duration_ms: 1,
        exit_code: 2,
        status: CommandStatus::Failed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let blocks = vec![block.clone()];
    let mut output = Vec::new();

    start_agent_for_block(
        &block,
        &blocks,
        &[],
        &adapter,
        &mut state,
        &mut output,
        FailedCommandAgentStartOptions {
            selectable_after_event_index: None,
            trigger: FailedCommandAnalysisTrigger::Auto,
        },
    )
    .expect("analysis start is gated, not errored");

    // The best-effort analysis was suppressed by the running compaction, so it
    // must not be permanently recorded as analyzed — otherwise it would never
    // be retried once compaction ends.
    assert!(
        !state.analyzed_blocks.contains(&block.id),
        "suppressed analysis was wrongly marked analyzed"
    );
    assert!(state.agent_run.active.is_none());
    assert!(state.agent_run.queued_requests.is_empty());
}

#[test]
fn agent_requests_receive_paused_notice_during_compaction() {
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let (mut state, _sender) = state_with_childless_compaction();
    let mut output = Vec::new();

    render_intercept_agent_guidance(
        &[natural_language_event("please analyze the memory usage")],
        &[],
        &adapter,
        &mut state,
        &mut output,
        0,
    )
    .expect("render paused notice");

    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("paused"), "{rendered}");
    assert!(state.agent_run.active.is_none());
}

#[test]
fn recommendation_parsing_is_strict_and_binds_to_a_session() {
    let mut state = InlineState::default();

    // A well-formed v1 payload records a pending automatic compaction.
    note_compaction_recommendation(
        &mut state,
        "00000000-0000-4000-8000-000000000000:7:2:180000:120000",
    );
    assert!(state.control.session().compaction().has_pending_auto());

    // Wrong field count, a non-UUID session id, and non-numeric fields all
    // fail closed and leave no pending recommendation.
    for malformed in [
        "00000000-0000-4000-8000-000000000000:7:2:180000", // too few
        "00000000-0000-4000-8000-000000000000:7:2:180000:1:extra", // too many
        "not-a-uuid:7:2:180000:120000",                    // bad id
        "00000000-0000-4000-8000-00000000000G:7:2:180000:120000", // non-hex
        "00000000-0000-4000-8000-000000000000:seven:2:180000:120000", // non-numeric
    ] {
        let mut fresh = InlineState::default();
        note_compaction_recommendation(&mut fresh, malformed);
        assert!(
            !fresh.control.session().compaction().has_pending_auto(),
            "malformed payload accepted: {malformed}"
        );
    }
}

/// Minimal user/analysis request for gate tests.
fn gate_request(id: &str) -> crate::types::AgentRequest {
    use crate::types::{AgentMode, AgentRequest, CommandBlock, CommandStatus, OutputRefs};
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
        user_input: Some("please help".to_string()),
        findings: Vec::new(),
        mode: AgentMode::RecommendOnly,
        user_confirmed: true,
        hook_finding: None,
        recommended_skill: None,
    }
}

const RECOMMENDATION: &str = "00000000-0000-4000-8000-000000000000:1:0:200000:100000";

#[test]
fn internal_run_suppressed_while_compaction_recommended() {
    use crate::agent::run::{
        start_agent_run_with_origin_disposition, AgentRunOrigin, AgentStartDisposition,
        AgentStartIntent,
    };
    // A recommendation is pending but no compactor is running yet.
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let mut state = InlineState::default();
    note_compaction_recommendation(&mut state, RECOMMENDATION);
    assert!(state.control.session().compaction().has_pending_auto());

    let mut output = Vec::new();
    let disposition = start_agent_run_with_origin_disposition(
        &gate_request("auto-1"),
        AgentRunOrigin::AutoFailure,
        AgentStartIntent::InternalBestEffort,
        &adapter,
        &mut state,
        &mut output,
        None,
    )
    .expect("gate returns Ok");

    // The internal continuation is dropped, nothing starts or queues, and the
    // recommendation survives so the compactor can still start at the next
    // idle poll.
    assert_eq!(disposition, AgentStartDisposition::SuppressedByCompaction);
    assert!(state.agent_run.active.is_none());
    assert!(state.agent_run.queued_requests.is_empty());
    assert!(state.control.session().compaction().has_pending_auto());
}

#[test]
fn user_run_queued_while_compaction_recommended() {
    use crate::agent::run::{
        start_agent_run_with_origin_disposition, AgentRunOrigin, AgentStartDisposition,
        AgentStartIntent,
    };
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let mut state = InlineState::default();
    note_compaction_recommendation(&mut state, RECOMMENDATION);

    let mut output = Vec::new();
    let disposition = start_agent_run_with_origin_disposition(
        &gate_request("user-1"),
        AgentRunOrigin::Standard,
        AgentStartIntent::UserInitiated,
        &adapter,
        &mut state,
        &mut output,
        None,
    )
    .expect("gate returns Ok");

    // The explicit user request is preserved (queued), not dropped, and the
    // recommendation is left intact so the compaction still runs first.
    assert_eq!(disposition, AgentStartDisposition::Queued);
    assert_eq!(state.agent_run.queued_requests.len(), 1);
    assert!(state.control.session().compaction().has_pending_auto());
}

#[test]
fn user_requests_queue_fifo_during_active_compaction() {
    use crate::agent::run::{
        start_agent_run_with_origin_disposition, AgentRunOrigin, AgentStartDisposition,
        AgentStartIntent,
    };
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let (mut state, _sender) = state_with_childless_compaction();
    let mut output = Vec::new();

    for id in ["user-1", "user-2"] {
        let disposition = start_agent_run_with_origin_disposition(
            &gate_request(id),
            AgentRunOrigin::Standard,
            AgentStartIntent::UserInitiated,
            &adapter,
            &mut state,
            &mut output,
            None,
        )
        .expect("gate returns Ok");
        assert_eq!(disposition, AgentStartDisposition::Queued);
    }
    // Both user requests are queued in arrival order; nothing started.
    assert!(state.agent_run.active.is_none());
    let ids: Vec<&str> = state
        .agent_run
        .queued_requests
        .iter()
        .map(|pending| pending.request.id.as_str())
        .collect();
    assert_eq!(ids, ["user-1", "user-2"]);
}

#[test]
fn auto_failure_suppresses_the_session_scoped_revision_marker() {
    // An automatic attempt that fails records a suppression marker carrying
    // the session id, so the same revision on the same session will not
    // retrigger — but a different session is unaffected.
    let (sender, receiver) = mpsc::channel();
    let mut active = childless_active(receiver);
    active.origin = CompactionOrigin::Auto;
    active.revision_marker = Some(super::process::SuppressionMarker {
        session_id: "00000000-0000-4000-8000-000000000000".to_string(),
        generation: 1,
        projection_revision: 0,
    });
    let mut state = InlineState::default();
    state.control.session_mut().compaction_mut().active = Some(active);
    sender
        .send(CompactionOutcome::Failed {
            code: "provider_error".to_string(),
            message: "boom".to_string(),
        })
        .expect("queue outcome");

    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");

    let compaction = state.control.session().compaction();
    assert!(
        compaction.is_auto_marker_suppressed(&super::process::SuppressionMarker {
            session_id: "00000000-0000-4000-8000-000000000000".to_string(),
            generation: 1,
            projection_revision: 0,
        }),
        "failed revision was not suppressed"
    );
    // A different session with the same generation/revision is a distinct
    // identity and must not be suppressed by this failure.
    assert!(
        !compaction.is_auto_marker_suppressed(&super::process::SuppressionMarker {
            session_id: "11111111-1111-4111-8111-111111111111".to_string(),
            generation: 1,
            projection_revision: 0,
        }),
        "a different session was wrongly suppressed"
    );
}

#[test]
fn completion_is_rendered_before_a_held_user_request_resumes() {
    use crate::agent::run::{AgentRunOrigin, AgentStartIntent, PendingAgentRequest};

    // A user request was held back in the queue while the compaction ran.
    let (mut state, sender) = state_with_childless_compaction();
    state
        .agent_run
        .queued_requests
        .push_back(PendingAgentRequest {
            request: gate_request("held-user"),
            origin: AgentRunOrigin::Standard,
            intent: AgentStartIntent::UserInitiated,
            class: crate::agent::run::PendingRequestClass::Normal,
            selectable_after_event_index: None,
            before_held_text: false,
        });
    sender
        .send(CompactionOutcome::Committed {
            tokens_before: 74_210,
            tokens_after: 29_800,
            after_source: "estimated".to_string(),
        })
        .expect("queue outcome");

    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");

    // The completion notice is rendered, and the held user request is resumed
    // (dequeued) in the same safe-boundary pass — a later input cannot jump
    // ahead of it because resume runs before any new input is handled.
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("74210"), "{rendered}");
    assert!(state.agent_run.queued_requests.is_empty());
}

#[test]
fn pending_completion_blocks_agent_start_until_rendered() {
    use crate::agent::run::{
        start_agent_run_with_origin_disposition, AgentRunOrigin, AgentStartDisposition,
        AgentStartIntent,
    };

    let (mut state, sender) = state_with_childless_compaction();
    sender
        .send(CompactionOutcome::Committed {
            tokens_before: 74_210,
            tokens_after: 29_800,
            after_source: "estimated".to_string(),
        })
        .expect("queue outcome");
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);

    // A busy poll harvests the finished compactor (active -> pending_completion)
    // but must not render it yet.
    let mut busy_output = Vec::new();
    poll_background_compaction(&mut state, &mut busy_output, &adapter, true).expect("busy poll");
    assert!(busy_output.is_empty());
    assert!(!compaction_active(&state));
    assert!(state
        .control
        .session()
        .compaction()
        .has_pending_completion());

    // While the completion is pending, the gate still pauses: internal work is
    // suppressed and a user request is queued — nothing starts.
    let mut gate_output = Vec::new();
    let internal = start_agent_run_with_origin_disposition(
        &gate_request("auto"),
        AgentRunOrigin::AutoFailure,
        AgentStartIntent::InternalBestEffort,
        &adapter,
        &mut state,
        &mut gate_output,
        None,
    )
    .expect("gate");
    assert_eq!(internal, AgentStartDisposition::SuppressedByCompaction);
    let user = start_agent_run_with_origin_disposition(
        &gate_request("user"),
        AgentRunOrigin::Standard,
        AgentStartIntent::UserInitiated,
        &adapter,
        &mut state,
        &mut gate_output,
        None,
    )
    .expect("gate");
    assert_eq!(user, AgentStartDisposition::Queued);
    assert!(state.agent_run.active.is_none());

    // The safe-boundary poll renders the completion, then resumes the queued
    // user request (FIFO); the pending completion is cleared.
    let mut safe_output = Vec::new();
    poll_background_compaction(&mut state, &mut safe_output, &adapter, false).expect("safe poll");
    let rendered = String::from_utf8(safe_output).expect("UTF-8");
    assert!(rendered.contains("74210"), "{rendered}");
    assert!(!state
        .control
        .session()
        .compaction()
        .has_pending_completion());
    assert!(state.agent_run.queued_requests.is_empty());
}

#[test]
fn natural_language_during_compaction_enqueues_once_then_resumes() {
    // A running compaction whose result has not arrived yet keeps the Agent
    // paused (the receiver stays empty until we send an outcome).
    let (mut state, sender) = state_with_childless_compaction();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let events = [natural_language_event("please analyze the memory usage")];

    // Submitting the same intercept event twice enqueues exactly one request
    // (dedup) and shows the paused notice — never started, never sent to bash.
    let mut output = Vec::new();
    render_intercept_agent_guidance(&events, &[], &adapter, &mut state, &mut output, 0)
        .expect("first submit");
    render_intercept_agent_guidance(&events, &[], &adapter, &mut state, &mut output, 0)
        .expect("duplicate submit");
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("paused"), "{rendered}");
    assert_eq!(state.agent_run.queued_requests.len(), 1);
    assert!(state.agent_run.active.is_none());

    // The compaction completes; a safe-boundary poll resumes the single queued
    // user request in order.
    sender
        .send(CompactionOutcome::Committed {
            tokens_before: 74_210,
            tokens_after: 29_800,
            after_source: "estimated".to_string(),
        })
        .expect("queue outcome");
    let mut resume_output = Vec::new();
    poll_background_compaction(&mut state, &mut resume_output, &adapter, false)
        .expect("resume poll");
    assert!(state.agent_run.queued_requests.is_empty());
}

#[test]
fn path_prompt_during_compaction_renders_one_request_notice() {
    let (mut state, _sender) = state_with_childless_compaction();
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let input = "read ./README.md";
    let events = [path_prompt_event(input)];
    let mut output = Vec::new();

    render_intercept_agent_guidance(&events, &[], &adapter, &mut state, &mut output, 0)
        .expect("first submit");
    render_intercept_agent_guidance(&events, &[], &adapter, &mut state, &mut output, 0)
        .expect("duplicate submit");

    let rendered = String::from_utf8(output).expect("UTF-8");
    assert_eq!(
        rendered.matches(&format!("Agent input: {input}")).count(),
        1
    );
    assert!(rendered.contains("paused"), "{rendered}");
    assert_eq!(state.agent_run.queued_requests.len(), 1);
    assert!(state.agent_run.active.is_none());
}

#[test]
fn path_prompt_queue_full_notice_follows_request_notice() {
    let (mut state, _sender) = state_with_childless_compaction();
    fill_user_queue_to_capacity(&mut state);
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let input = "read ./README.md";
    let events = [path_prompt_event(input)];
    let mut output = Vec::new();

    render_intercept_agent_guidance(&events, &[], &adapter, &mut state, &mut output, 0)
        .expect("reject full queue");

    let rendered = String::from_utf8(output).expect("UTF-8");
    let request = rendered.find("Agent input:").expect("request notice");
    let rejected = rendered.find("Too many").expect("queue-full notice");
    assert!(request < rejected, "{rendered}");
    assert!(state.agent_run.active.is_none());
}

#[test]
fn user_request_is_rejected_with_notice_when_queue_is_full() {
    use crate::agent::queue::MAX_QUEUED_AGENT_REQUESTS;
    use crate::agent::run::{
        start_agent_run_with_origin_disposition, AgentRunOrigin, AgentStartDisposition,
        AgentStartIntent, PendingAgentRequest,
    };

    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let (mut state, _sender) = state_with_childless_compaction();
    // Fill the queue to capacity.
    for index in 0..MAX_QUEUED_AGENT_REQUESTS {
        state
            .agent_run
            .queued_requests
            .push_back(PendingAgentRequest {
                request: gate_request(&format!("queued-{index}")),
                origin: AgentRunOrigin::Standard,
                intent: AgentStartIntent::UserInitiated,
                class: crate::agent::run::PendingRequestClass::Normal,
                selectable_after_event_index: None,
                before_held_text: false,
            });
    }
    let mut output = Vec::new();
    let disposition = start_agent_run_with_origin_disposition(
        &gate_request("overflow"),
        AgentRunOrigin::Standard,
        AgentStartIntent::UserInitiated,
        &adapter,
        &mut state,
        &mut output,
        None,
    )
    .expect("gate");
    // The overflow request is rejected (not silently dropped, not started) and
    // the queue does not grow past capacity.
    assert_eq!(disposition, AgentStartDisposition::QueueFull);
    assert_eq!(
        state.agent_run.queued_requests.len(),
        MAX_QUEUED_AGENT_REQUESTS
    );
    assert!(state.agent_run.active.is_none());
}

fn fill_user_queue_to_capacity(state: &mut InlineState) {
    use crate::agent::queue::MAX_QUEUED_AGENT_REQUESTS;
    use crate::agent::run::{AgentRunOrigin, AgentStartIntent, PendingAgentRequest};
    for index in 0..MAX_QUEUED_AGENT_REQUESTS {
        state
            .agent_run
            .queued_requests
            .push_back(PendingAgentRequest {
                request: gate_request(&format!("queued-{index}")),
                origin: AgentRunOrigin::Standard,
                intent: AgentStartIntent::UserInitiated,
                class: crate::agent::run::PendingRequestClass::Normal,
                selectable_after_event_index: None,
                before_held_text: false,
            });
    }
}

#[test]
fn wrapper_surfaces_queue_full_notice_for_user_request() {
    use crate::agent::queue::MAX_QUEUED_AGENT_REQUESTS;
    use crate::agent::run::{start_agent_run_with_origin, AgentRunOrigin, AgentStartIntent};

    // Even the disposition-discarding wrapper must not silently lose a user
    // request: a full queue during compaction produces a visible notice.
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let (mut state, _sender) = state_with_childless_compaction();
    fill_user_queue_to_capacity(&mut state);
    let mut output = Vec::new();

    start_agent_run_with_origin(
        &gate_request("overflow"),
        AgentRunOrigin::Standard,
        AgentStartIntent::UserInitiated,
        &adapter,
        &mut state,
        &mut output,
        None,
    )
    .expect("wrapper returns Ok");

    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("Too many"), "{rendered}");
    assert_eq!(
        state.agent_run.queued_requests.len(),
        MAX_QUEUED_AGENT_REQUESTS
    );
    assert!(state.agent_run.active.is_none());
}

#[test]
fn queue_full_notice_title_depends_on_compaction() {
    // While a compaction is pausing the Agent, the compaction-framed title is
    // used.
    let (state, _sender) = state_with_childless_compaction();
    let mut output = Vec::new();
    render_agent_queue_full_notice(&state, &mut output).expect("render");
    let compacting = String::from_utf8(output).expect("UTF-8");
    assert!(compacting.contains("compaction"), "{compacting}");

    // With no compaction (an ordinary busy Agent), the dedicated queue-full
    // title is used — not the "queued" title (the request was NOT queued) and
    // not a false "compaction" claim.
    let idle = InlineState::default();
    let mut output = Vec::new();
    render_agent_queue_full_notice(&idle, &mut output).expect("render");
    let generic = String::from_utf8(output).expect("UTF-8");
    assert!(generic.contains("Agent queue full"), "{generic}");
    assert!(!generic.contains("compaction"), "{generic}");
}

#[test]
fn control_protocol_response_is_guaranteed_a_queue_slot() {
    use crate::agent::queue::MAX_QUEUED_AGENT_REQUESTS;
    use crate::agent::run::{start_agent_run_control_response, AgentRunOrigin};

    // A control-protocol response (question answer / approval resolution) has
    // already consumed conversation state, so it must never be rejected by a
    // full queue: it bypasses the cap and is always accepted.
    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let (mut state, _sender) = state_with_childless_compaction();
    fill_user_queue_to_capacity(&mut state);
    assert_eq!(
        state.agent_run.queued_requests.len(),
        MAX_QUEUED_AGENT_REQUESTS
    );
    let mut output = Vec::new();

    start_agent_run_control_response(
        &gate_request("question-answer"),
        AgentRunOrigin::Standard,
        &adapter,
        &mut state,
        &mut output,
        None,
    )
    .expect("control response accepted");

    // Accepted past the cap (queued, not rejected) and not started.
    assert_eq!(
        state.agent_run.queued_requests.len(),
        MAX_QUEUED_AGENT_REQUESTS + 1
    );
    assert!(state
        .agent_run
        .queued_requests
        .iter()
        .any(|pending| pending.request.id == "question-answer"));
    assert!(state.agent_run.active.is_none());
    // No queue-full notice was shown for the guaranteed response.
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(!rendered.contains("Too many"), "{rendered}");
}

#[test]
fn user_confirmed_analysis_queue_full_reverts_and_notifies() {
    use crate::agent::failed_command::{
        start_agent_for_block, FailedCommandAgentStartOptions, FailedCommandAnalysisTrigger,
    };
    use crate::runtime::state::AnalysisMode;
    use crate::types::{CommandBlock, CommandStatus, OutputRefs};

    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let (mut state, _sender) = state_with_childless_compaction();
    state.analysis_mode = AnalysisMode::Auto;
    fill_user_queue_to_capacity(&mut state);
    let block = CommandBlock {
        id: "blk-userconfirmed".to_string(),
        session_id: "shell-session".to_string(),
        command: "make release".to_string(),
        origin: Default::default(),
        cwd: "/repo".to_string(),
        end_cwd: "/repo".to_string(),
        started_at_ms: 1,
        ended_at_ms: 2,
        duration_ms: 1,
        exit_code: 2,
        status: CommandStatus::Failed,
        output: OutputRefs {
            terminal_output_ref: None,
            terminal_output_bytes: 0,
        },
        shell_environment_generation: None,
        audit_identity: None,
    };
    let blocks = vec![block.clone()];
    let mut output = Vec::new();

    start_agent_for_block(
        &block,
        &blocks,
        &[],
        &adapter,
        &mut state,
        &mut output,
        FailedCommandAgentStartOptions {
            selectable_after_event_index: None,
            trigger: FailedCommandAnalysisTrigger::UserConfirmed,
        },
    )
    .expect("analysis start is gated, not errored");

    // The queue was full, so the analysis neither ran nor queued: its dedup
    // mark is reverted and the rejection is surfaced (not silent).
    assert!(!state.analyzed_blocks.contains(&block.id));
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("Too many"), "{rendered}");
}

#[test]
fn success_envelope_missing_or_malformed_fields_is_a_protocol_failure() {
    // ok=true with no data must not be read as a 0 -> 0 success.
    match parse_compactor_output(r#"{"ok":true}"#) {
        CompactionOutcome::Failed { code, .. } => assert_eq!(code, "protocol"),
        CompactionOutcome::Committed { .. } => panic!("missing data accepted"),
    }
    // Wrong value type.
    let wrong_type = r#"{"ok":true,"data":{"tokens_before":{"value":100,"source":"x"},"tokens_after":{"value":"oops","source":"estimated"}}}"#;
    match parse_compactor_output(wrong_type) {
        CompactionOutcome::Failed { code, .. } => assert_eq!(code, "protocol"),
        CompactionOutcome::Committed { .. } => panic!("wrong-typed value accepted"),
    }
    // Empty tokens_after.source.
    let empty_source = r#"{"ok":true,"data":{"tokens_before":{"value":100,"source":"x"},"tokens_after":{"value":30,"source":"  "}}}"#;
    match parse_compactor_output(empty_source) {
        CompactionOutcome::Failed { code, .. } => assert_eq!(code, "protocol"),
        CompactionOutcome::Committed { .. } => panic!("empty source accepted"),
    }
    // Missing tokens_after entirely.
    let missing_after = r#"{"ok":true,"data":{"tokens_before":{"value":100,"source":"x"}}}"#;
    match parse_compactor_output(missing_after) {
        CompactionOutcome::Failed { code, .. } => assert_eq!(code, "protocol"),
        CompactionOutcome::Committed { .. } => panic!("missing tokens_after accepted"),
    }
}

#[test]
fn compactor_output_parsing_covers_success_error_and_garbage() {
    let success = r#"{"ok":true,"data":{"tokens_before":{"value":100,"source":"provider_reported"},"tokens_after":{"value":30,"source":"estimated"}}}"#;
    match parse_compactor_output(success) {
        CompactionOutcome::Committed {
            tokens_before,
            tokens_after,
            after_source,
        } => {
            assert_eq!(tokens_before, 100);
            assert_eq!(tokens_after, 30);
            assert_eq!(after_source, "estimated");
        }
        CompactionOutcome::Failed { .. } => panic!("expected committed outcome"),
    }

    let failure = r#"{"ok":false,"error":{"code":"conflict","message":"session changed"}}"#;
    match parse_compactor_output(failure) {
        CompactionOutcome::Failed { code, message } => {
            assert_eq!(code, "conflict");
            assert_eq!(message, "session changed");
        }
        CompactionOutcome::Committed { .. } => panic!("expected failed outcome"),
    }

    match parse_compactor_output("not json at all\n中文噪声") {
        CompactionOutcome::Failed { code, .. } => assert_eq!(code, "transport"),
        CompactionOutcome::Committed { .. } => panic!("expected transport failure"),
    }
}

#[test]
fn bounded_error_text_is_utf8_safe() {
    let long = "错误信息".repeat(400);
    let bounded_text = bounded(&long);
    assert!(bounded_text.chars().count() <= MAX_REPORTED_ERROR_CHARS + 1);
    assert!(bounded_text.ends_with('…'));
    assert!(bounded("short ok").ends_with("ok"));
}

#[test]
fn untrusted_error_fields_are_sanitized_before_display() {
    // A hostile compactor controls every error field: the code must be
    // forced into bounded snake_case and the message must lose secrets and
    // control sequences before it can reach the terminal.
    let hostile = concat!(
        r#"{"ok":false,"error":{"code":"CONFLICT!! \u001b[31m$(rm -rf) "#,
        r#"very_long_code_padding_padding_padding_padding","#,
        r#""message":"boom api_key=sk-secret-value-123456 \u001b[2J wiped"}}"#
    );
    match parse_compactor_output(hostile) {
        CompactionOutcome::Failed { code, message } => {
            assert!(
                code.chars()
                    .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_'),
                "{code}"
            );
            assert!(code.chars().count() <= 32, "{code}");
            // The panel renders `bounded(message)`: secrets and control
            // characters must not survive it.
            let shown = bounded(&message);
            assert!(!shown.contains("sk-secret-value-123456"), "{shown}");
            assert!(shown.contains("<redacted>"), "{shown}");
            assert!(!shown.contains('\u{1b}'), "{shown}");
        }
        CompactionOutcome::Committed { .. } => panic!("hostile envelope accepted as success"),
    }

    // A code with no acceptable characters falls back to `protocol`.
    match parse_compactor_output(r#"{"ok":false,"error":{"code":"!!! ###","message":"x"}}"#) {
        CompactionOutcome::Failed { code, .. } => assert_eq!(code, "protocol"),
        CompactionOutcome::Committed { .. } => panic!("garbage code accepted"),
    }
}

#[test]
fn control_characters_cannot_split_secrets_past_redaction() {
    // Controls must be normalized BEFORE redaction: a NUL inside the key
    // name would otherwise hide `api_key` from the patterns, and the later
    // display-side filtering would reassemble a fully readable secret.
    let shown = bounded("api_\u{0}key=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");

    let shown = bounded("tok\u{0}en=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");

    // A whole ANSI sequence inside the key name: the full CSI sequence must
    // vanish (no printable `[31m` residue) and must not shield the secret.
    let shown = bounded("api_\u{1b}[31mkey=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(!shown.contains("[31m"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
}

#[test]
fn control_characters_cannot_split_a_private_key_marker() {
    // NULs split the BEGIN marker and replace the newlines; after control
    // normalization the canonical single-line block must still be caught by
    // the private-key redaction.
    let payload =
        "-----BEGIN\u{0} PRIVATE KEY-----\u{0}MIIsecretkeymaterial\u{0}-----END PRIVATE KEY-----";
    let shown = bounded(payload);
    assert!(!shown.contains("MIIsecretkeymaterial"), "{shown}");
    assert!(shown.contains("redacted"), "{shown}");
}

#[test]
fn invisible_unicode_format_characters_cannot_split_secrets() {
    // Unicode Cf / default-ignorable characters are not `char::is_control()`
    // but render as nothing: they must be removed BEFORE redaction, or the
    // terminal shows an ordinary `api_key=` assignment that the patterns
    // never matched.
    let shown = bounded("api_\u{200B}key=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");

    let shown = bounded("tok\u{2060}en=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");

    // Bidi controls (override + isolate) inside the key name.
    let shown = bounded("api_\u{202E}key=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
    assert!(!shown.contains('\u{202E}'), "{shown}");
    let shown = bounded("api_\u{2066}key\u{2069}=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
}

#[test]
fn reserved_default_ignorable_code_points_cannot_split_secrets() {
    // Reserved code points inside the normative Default_Ignorable ranges
    // (DerivedCoreProperties.txt) — e.g. U+2065, U+FFF0..U+FFF8, and the
    // unassigned plane-14 gaps — render as nothing on conforming terminals
    // and previously slipped through the hand-written table. Probe the exact
    // boundaries of every gap the table used to have.
    for gap in [
        '\u{2065}',
        '\u{FFF0}',
        '\u{FFF8}',
        '\u{E0080}',
        '\u{E00FF}',
        '\u{E01F0}',
        '\u{E0FFF}',
    ] {
        let shown = bounded(&format!("api_{gap}key=plain-secret-value"));
        assert!(
            !shown.contains("plain-secret-value"),
            "U+{:04X}: {shown}",
            gap as u32
        );
        assert!(
            shown.contains("<redacted>"),
            "U+{:04X}: {shown}",
            gap as u32
        );
    }
}

#[test]
fn bare_esc_inside_string_sequences_does_not_release_the_payload() {
    // A bare ESC (not ST) inside an OSC payload stays part of the payload:
    // consumption continues to the real BEL terminator, so the key is
    // reassembled for redaction and no payload fragment leaks.
    let shown = bounded("api_\u{1b}]0;ti\u{1b}tle\u{7}key=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(!shown.contains("tle"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");

    // An embedded introducer (ESC [) inside the payload is payload too, not
    // a fresh sequence that would resume ordinary output afterwards.
    let shown = bounded("\u{1b}]x\u{1b}[31mleak-osc-secret\u{7}visible tail");
    assert!(!shown.contains("leak-osc-secret"), "{shown}");
    assert!(!shown.contains("31m"), "{shown}");
    assert!(shown.contains("visible tail"), "{shown}");

    // A DCS payload with an embedded bare ESC and no ST consumes to the end
    // of input (fail closed): nothing after the introducer may appear.
    let shown = bounded("\u{1b}Pdcs\u{1b}payload-secret trailing-data");
    assert!(!shown.contains("payload-secret"), "{shown}");
    assert!(!shown.contains("trailing-data"), "{shown}");
}

#[test]
fn osc_and_dcs_sequences_are_fully_consumed_before_redaction() {
    // OSC terminated by BEL inside the key name: the whole sequence
    // (introducer AND payload) must vanish, restoring `api_key` for the
    // redaction patterns.
    let shown = bounded("api_\u{1b}]0;title\u{7}key=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(!shown.contains("0;title"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");

    // OSC terminated by ST (ESC \).
    let shown = bounded("api_\u{1b}]0;title\u{1b}\\key=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(!shown.contains("0;title"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");

    // OSC/DCS payloads must never leak into the output — not even when they
    // carry the secret themselves (an unterminated payload is consumed to
    // the end, fail closed).
    let shown = bounded("\u{1b}]0;osc-payload-secret\u{7}visible after osc");
    assert!(!shown.contains("osc-payload-secret"), "{shown}");
    assert!(shown.contains("visible after osc"), "{shown}");
    let shown = bounded("\u{1b}Pq-dcs-payload-secret\u{1b}\\visible after dcs");
    assert!(!shown.contains("dcs-payload-secret"), "{shown}");
    assert!(shown.contains("visible after dcs"), "{shown}");
    let shown = bounded("\u{1b}]unterminated-osc-secret api_key=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(!shown.contains("unterminated-osc-secret"), "{shown}");

    // C1 single-byte introducers behave like their ESC forms.
    let shown = bounded("api_\u{9b}31mkey=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(!shown.contains("31m"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
    let shown = bounded("api_\u{9d}0;title\u{7}key=plain-secret-value");
    assert!(!shown.contains("plain-secret-value"), "{shown}");
    assert!(!shown.contains("0;title"), "{shown}");
    assert!(shown.contains("<redacted>"), "{shown}");
}

#[test]
fn stderr_tail_secrets_are_redacted_in_the_failure_panel() {
    // Secrets captured from the compactor's stderr must be redacted before
    // the timeout failure renders them.
    let (mut state, _sender) = state_with_childless_compaction();
    {
        let compaction = state.control.session_mut().compaction_mut();
        let active = compaction.active.as_mut().expect("active");
        active
            .stderr_tail
            .push(b"fatal: token=ghp_secretsecretsecret1234 refused");
    }
    force_deadline_elapsed(&mut state);
    state.control.session_mut().compaction_mut().poll();
    force_grace_elapsed(&mut state);

    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");
    let text = normalized_panel_text(&String::from_utf8(output).expect("UTF-8"));
    assert!(!text.contains("ghp_secretsecretsecret1234"), "{text}");
    assert!(text.contains("<redacted>"), "{text}");
}

#[test]
fn unknown_token_source_fails_the_success_envelope() {
    // `tokens_after.source` is rendered verbatim, so only the known protocol
    // labels are accepted; anything else is a protocol failure, never text
    // that flows into the completion panel.
    let unknown = r#"{"ok":true,"data":{"tokens_before":{"value":100,"source":"provider_reported"},"tokens_after":{"value":30,"source":"attacker controlled text"}}}"#;
    match parse_compactor_output(unknown) {
        CompactionOutcome::Failed { code, .. } => assert_eq!(code, "protocol"),
        CompactionOutcome::Committed { .. } => panic!("unknown source accepted"),
    }
}

fn force_deadline_elapsed(state: &mut InlineState) {
    state
        .control
        .session_mut()
        .compaction_mut()
        .active
        .as_mut()
        .expect("active compaction")
        .deadline = Instant::now() - std::time::Duration::from_millis(1);
}

fn force_grace_elapsed(state: &mut InlineState) {
    state
        .control
        .session_mut()
        .compaction_mut()
        .active
        .as_mut()
        .expect("active compaction")
        .termination
        .as_mut()
        .expect("termination in flight")
        .kill_at = Instant::now() - std::time::Duration::from_millis(1);
}

/// Flattens rendered panel output for semantic assertions: box-drawing
/// borders become spaces and wrapped whitespace collapses, so checks never
/// depend on the renderer's terminal width.
fn normalized_panel_text(rendered: &str) -> String {
    rendered
        .chars()
        .map(|ch| match ch {
            '│' | '╭' | '╮' | '╰' | '╯' | '─' => ' ',
            other => other,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn deadline_timeout_yields_one_typed_completion_and_resumes_queue() {
    use crate::agent::run::{AgentRunOrigin, AgentStartIntent, PendingAgentRequest};

    // A compactor that never produces a result: the sender stays open and
    // silent, exactly like a hung provider call.
    let (mut state, _sender) = state_with_childless_compaction();
    state
        .agent_run
        .queued_requests
        .push_back(PendingAgentRequest {
            request: gate_request("held-during-timeout"),
            origin: AgentRunOrigin::Standard,
            intent: AgentStartIntent::UserInitiated,
            class: crate::agent::run::PendingRequestClass::Normal,
            selectable_after_event_index: None,
            before_held_text: false,
        });
    {
        let compaction = state.control.session_mut().compaction_mut();
        let active = compaction.active.as_mut().expect("active");
        // Control characters in stderr must never reach the panel verbatim.
        active.stderr_tail.push(b"provider hung\x1b[31m mid-stream");
    }
    force_deadline_elapsed(&mut state);

    // First poll past the deadline: SIGTERM is requested, the grace window
    // opens, and the compaction is still active (not yet completed).
    state.control.session_mut().compaction_mut().poll();
    {
        let compaction = state.control.session_mut().compaction_mut();
        let active = compaction.active.as_ref().expect("still active in grace");
        let termination = active.termination.expect("termination armed");
        assert!(!active.cancel_requested(), "timeout is not a user cancel");
        assert!(termination.kill_at <= Instant::now() + TERMINATION_GRACE);
    }

    // Grace expires with no result: the next safe-boundary poll finishes the
    // termination state machine, renders exactly one typed timeout failure,
    // and resumes the held user request — the Agent gate is fully reopened.
    force_grace_elapsed(&mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("boundary poll");
    let rendered = String::from_utf8(output).expect("UTF-8");
    // The renderer may wrap the panel at any terminal width; compare
    // whitespace-normalized text so the assertions stay layout-independent.
    let text = normalized_panel_text(&rendered);
    assert!(text.contains("timeout"), "{rendered}");
    assert!(text.contains("provider hung"), "{rendered}");
    assert!(
        !rendered.contains('\u{1b}'),
        "control chars must be stripped"
    );
    assert!(!compaction_active(&state));
    assert!(!state
        .control
        .session()
        .compaction()
        .has_pending_completion());
    assert!(!compaction_pending_or_active(&state) || state.agent_run.active.is_some());
    assert!(state.agent_run.queued_requests.is_empty(), "queue resumed");

    // The terminal completion is consumed exactly once: nothing renders again.
    let mut second = Vec::new();
    poll_background_compaction(&mut state, &mut second, &adapter, false).expect("second poll");
    assert!(String::from_utf8(second)
        .expect("UTF-8")
        .replace(|ch: char| ch.is_whitespace(), "")
        .is_empty());
}

#[test]
fn cancel_grace_expiry_renders_cancelled_once() {
    let (mut state, _sender) = state_with_childless_compaction();
    let mut cancel_output = Vec::new();
    cancel_compaction(&mut state, &mut cancel_output).expect("cancel");
    assert!(String::from_utf8(cancel_output)
        .expect("UTF-8")
        .contains("Cancellation requested"));
    {
        let compaction = state.control.session_mut().compaction_mut();
        let active = compaction.active.as_ref().expect("active");
        assert!(active.cancel_requested());
    }

    // The grace period elapses without a result (the compactor never writes
    // one): the poll escalates past the grace window and reports `cancelled`.
    // Real SIGKILL delivery and reaping are verified in raw_cli.
    force_grace_elapsed(&mut state);
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("boundary poll");
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("cancelled"), "{rendered}");
    assert!(rendered.contains("transcript is unchanged"), "{rendered}");
    assert!(!compaction_active(&state));

    // Exactly-once: a second poll renders nothing further.
    let mut second = Vec::new();
    poll_background_compaction(&mut state, &mut second, &adapter, false).expect("second poll");
    assert!(String::from_utf8(second)
        .expect("UTF-8")
        .replace(|ch: char| ch.is_whitespace(), "")
        .is_empty());
}

#[test]
fn result_arriving_inside_the_grace_window_is_still_honoured() {
    // SIGTERM was sent (deadline), but the compactor manages to flush a real
    // committed envelope before the grace expires: report the truth, not a
    // synthesized timeout.
    let (mut state, sender) = state_with_childless_compaction();
    force_deadline_elapsed(&mut state);
    state.control.session_mut().compaction_mut().poll();
    sender
        .send(CompactionOutcome::Committed {
            tokens_before: 50_000,
            tokens_after: 20_000,
            after_source: "estimated".to_string(),
        })
        .expect("late result");

    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("50000"), "{rendered}");
    assert!(!rendered.contains("timeout"), "{rendered}");
}

#[test]
fn deadline_exit_transport_result_is_reclassified_as_timeout() {
    // The COMMON deadline path: SIGTERM makes a well-behaved compactor exit,
    // the stdout reader hits EOF and parses nothing — a `transport` result.
    // That exit is a consequence of the termination, so the completion must
    // carry the promised typed `timeout`, never `transport`.
    let (mut state, sender) = state_with_childless_compaction();
    force_deadline_elapsed(&mut state);
    state.control.session_mut().compaction_mut().poll(); // arms SIGTERM
    sender
        .send(CompactionOutcome::Failed {
            code: "transport".to_string(),
            message: "compactor produced no parseable result".to_string(),
        })
        .expect("reader EOF result");

    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");
    let text = normalized_panel_text(&String::from_utf8(output).expect("UTF-8"));
    assert!(text.contains("timeout"), "{text}");
    assert!(!text.contains("transport"), "{text}");
    assert!(!compaction_active(&state));
    assert!(!state
        .control
        .session()
        .compaction()
        .has_pending_completion());
}

#[test]
fn deadline_exit_reader_disconnect_is_reclassified_as_timeout() {
    // Same path, but the reader thread vanishes (channel disconnect) after
    // the terminated child goes away: still `timeout`, never `transport`.
    let (mut state, sender) = state_with_childless_compaction();
    force_deadline_elapsed(&mut state);
    state.control.session_mut().compaction_mut().poll(); // arms SIGTERM
    drop(sender);

    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");
    let text = normalized_panel_text(&String::from_utf8(output).expect("UTF-8"));
    assert!(text.contains("timeout"), "{text}");
    assert!(!text.contains("transport"), "{text}");
    assert!(!compaction_active(&state));
}

#[test]
fn structured_engine_failure_during_grace_is_reported_verbatim() {
    // A structured envelope failure that arrives inside the grace window is
    // real engine output (the compactor DID answer before dying); it must
    // keep its own code instead of being rewritten into `timeout`.
    let (mut state, sender) = state_with_childless_compaction();
    force_deadline_elapsed(&mut state);
    state.control.session_mut().compaction_mut().poll(); // arms SIGTERM
    sender
        .send(CompactionOutcome::Failed {
            code: "conflict".to_string(),
            message: "session changed concurrently".to_string(),
        })
        .expect("late structured failure");

    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");
    let text = normalized_panel_text(&String::from_utf8(output).expect("UTF-8"));
    assert!(text.contains("conflict"), "{text}");
    assert!(!text.contains("timeout"), "{text}");
}

#[test]
fn pending_completion_blocks_session_mutations_until_rendered() {
    let (mut state, sender) = state_with_childless_compaction();
    sender
        .send(CompactionOutcome::Committed {
            tokens_before: 74_210,
            tokens_after: 29_800,
            after_source: "estimated".to_string(),
        })
        .expect("queue outcome");
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);

    // Busy poll: active -> pending_completion without rendering.
    let mut busy = Vec::new();
    poll_background_compaction(&mut state, &mut busy, &adapter, true).expect("busy poll");
    assert!(!compaction_active(&state));
    assert!(state
        .control
        .session()
        .compaction()
        .has_pending_completion());

    // Session mutations must stay blocked across the whole window.
    assert!(!super::super::panel::session_management_idle(&state));

    // A fresh `/session compact` is a duplicate of in-flight work.
    let core = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    let mut duplicate = Vec::new();
    render_session_compact_command(None, &[], &core, &mut state, &mut duplicate)
        .expect("duplicate notice");
    assert!(String::from_utf8(duplicate)
        .expect("UTF-8")
        .contains("already running"));

    // Status must not claim idle while the completion awaits rendering.
    let mut status = Vec::new();
    render_compaction_status(&state, &mut status).expect("status");
    let status_text = String::from_utf8(status).expect("UTF-8");
    assert!(
        !status_text.contains("No background compaction"),
        "{status_text}"
    );
    assert!(
        status_text.contains("safe prompt boundary"),
        "{status_text}"
    );

    // After the completion is consumed at a safe boundary, mutations reopen.
    let mut safe = Vec::new();
    poll_background_compaction(&mut state, &mut safe, &adapter, false).expect("safe poll");
    assert!(String::from_utf8(safe).expect("UTF-8").contains("74210"));
    assert!(super::super::panel::session_management_idle(&state));
}

#[test]
fn recommended_compaction_blocks_mutations_but_not_its_own_start() {
    use crate::adapter::SessionRecoveryState;

    let adapter = AdapterInstance::CoshCore(CoshCoreAdapter {
        program: "/must-not-be-started".to_string(),
        ..CoshCoreAdapter::default()
    });
    if let AdapterInstance::CoshCore(core) = &adapter {
        let mut session = core
            .session
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        session.recovery.state = SessionRecoveryState::Selected;
        session.recovery.selected_session_id =
            Some("00000000-0000-4000-8000-000000000000".to_string());
        session.recovery.selected_workspace_scope = Some("/tmp".to_string());
    }
    let mut state = InlineState::default();
    note_compaction_recommendation(&mut state, RECOMMENDATION);

    // The full mutation gate blocks while an automatic attempt is pending…
    assert!(!super::super::panel::session_management_idle(&state));

    // …and the status reflects the recommendation instead of idle.
    let mut status = Vec::new();
    render_compaction_status(&state, &mut status).expect("status");
    assert!(String::from_utf8(status)
        .expect("UTF-8")
        .contains("idle boundary"));

    // …but the auto starter is not blocked by its own recommendation: it
    // consumes it and attempts the spawn (which fails on the bogus program
    // and records the per-revision suppression marker).
    let mut output = Vec::new();
    poll_background_compaction(&mut state, &mut output, &adapter, false).expect("poll");
    assert!(!state.control.session().compaction().has_pending_auto());
    assert!(state
        .control
        .session()
        .compaction()
        .is_auto_marker_suppressed(&super::process::SuppressionMarker {
            session_id: "00000000-0000-4000-8000-000000000000".to_string(),
            generation: 1,
            projection_revision: 0,
        }));
    assert!(String::from_utf8(output)
        .expect("UTF-8")
        .contains("Failed to start the background compactor"));
}

#[test]
fn cancel_pending_auto_releases_gate_and_suppresses_revision() {
    let mut state = InlineState::default();
    note_compaction_recommendation(&mut state, RECOMMENDATION);
    assert!(state.control.session().compaction().has_pending_auto());
    assert!(compaction_pending_or_active(&state));

    let mut output = Vec::new();
    cancel_compaction(&mut state, &mut output).expect("cancel pending recommendation");

    // The recommendation is removed atomically and the Agent compaction gate
    // is released immediately — no completion poll is needed first.
    assert!(!state.control.session().compaction().has_pending_auto());
    assert!(!compaction_pending_or_active(&state));
    assert!(super::super::panel::session_management_idle(&state));

    // The suppression marker binds the exact session + generation + revision
    // the cancelled recommendation named.
    assert!(state
        .control
        .session()
        .compaction()
        .is_auto_marker_suppressed(&super::process::SuppressionMarker {
            session_id: "00000000-0000-4000-8000-000000000000".to_string(),
            generation: 1,
            projection_revision: 0,
        }));

    // The notice is truthful: nothing was running, so it must not claim a
    // process is being terminated.
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(
        rendered.contains("Cancelled the recommended automatic compaction"),
        "{rendered}"
    );
    assert!(!rendered.contains("being terminated"), "{rendered}");
    assert!(!rendered.contains("Cancellation requested"), "{rendered}");
}

#[test]
fn cancelled_pending_revision_is_suppressed_but_new_revision_retriggers() {
    let mut state = InlineState::default();
    note_compaction_recommendation(&mut state, RECOMMENDATION);
    let mut output = Vec::new();
    cancel_compaction(&mut state, &mut output).expect("cancel pending recommendation");

    // A re-emitted status for the same session + generation + revision must
    // not re-arm the pending recommendation (or the Agent gate).
    note_compaction_recommendation(&mut state, RECOMMENDATION);
    assert!(!state.control.session().compaction().has_pending_auto());
    assert!(!compaction_pending_or_active(&state));

    // A new projection revision is a different identity and may trigger.
    note_compaction_recommendation(
        &mut state,
        "00000000-0000-4000-8000-000000000000:1:1:200000:100000",
    );
    assert!(state.control.session().compaction().has_pending_auto());
}

#[test]
fn cancel_pending_auto_resumes_queued_user_request_at_safe_boundary() {
    use crate::agent::run::{AgentRunOrigin, AgentStartIntent, PendingAgentRequest};

    // A user request was queued behind the recommended compaction.
    let mut state = InlineState::default();
    note_compaction_recommendation(&mut state, RECOMMENDATION);
    state
        .agent_run
        .queued_requests
        .push_back(PendingAgentRequest {
            request: gate_request("held-behind-recommendation"),
            origin: AgentRunOrigin::Standard,
            intent: AgentStartIntent::UserInitiated,
            class: crate::agent::run::PendingRequestClass::Normal,
            selectable_after_event_index: None,
            before_held_text: false,
        });

    let mut output = Vec::new();
    cancel_compaction(&mut state, &mut output).expect("cancel pending recommendation");

    // The next safe-boundary poll finds nothing pending or active and resumes
    // the held request — the user's intent is not lost with the cancellation.
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut resume_output = Vec::new();
    poll_background_compaction(&mut state, &mut resume_output, &adapter, false)
        .expect("resume poll");
    assert!(state.agent_run.queued_requests.is_empty(), "queue resumed");
}

#[test]
fn cancel_with_active_compactor_also_clears_pending_auto() {
    // A fresh recommendation can arrive while a compactor is already running;
    // one cancel must stop both, otherwise the pending attempt would restart
    // compaction right after the user cancelled it.
    let (mut state, _sender) = state_with_childless_compaction();
    note_compaction_recommendation(
        &mut state,
        "00000000-0000-4000-8000-000000000000:1:5:200000:100000",
    );

    let mut output = Vec::new();
    cancel_compaction(&mut state, &mut output).expect("cancel");

    // The running compactor is being terminated (and says so)…
    let rendered = String::from_utf8(output).expect("UTF-8");
    assert!(rendered.contains("Cancellation requested"), "{rendered}");
    assert!(state
        .control
        .session()
        .compaction()
        .active
        .as_ref()
        .expect("active")
        .cancel_requested());
    // …and the not-yet-started recommendation is removed and suppressed.
    assert!(!state.control.session().compaction().has_pending_auto());
    assert!(state
        .control
        .session()
        .compaction()
        .is_auto_marker_suppressed(&super::process::SuppressionMarker {
            session_id: "00000000-0000-4000-8000-000000000000".to_string(),
            generation: 1,
            projection_revision: 5,
        }));
}

#[test]
fn cancelled_pending_survives_active_auto_cancellation_completion() {
    // Regression: an automatic compactor (revision A) is running when a newer
    // recommendation (revision B) arrives. One `/session compact cancel` must
    // suppress BOTH — B immediately, A when its cancelled completion is
    // harvested — and the active compactor's completion must ADD its own
    // revision rather than clobber B's suppression. Otherwise re-emitting B
    // after the poll would restart the very compaction the user cancelled.
    const SESSION: &str = "00000000-0000-4000-8000-000000000000";
    let marker = |revision: u64| super::process::SuppressionMarker {
        session_id: SESSION.to_string(),
        generation: 1,
        projection_revision: revision,
    };

    // Active AUTO compactor bound to revision A (generation 1, revision 2).
    let (sender, receiver) = mpsc::channel();
    let mut active = childless_active(receiver);
    active.origin = CompactionOrigin::Auto;
    active.revision_marker = Some(marker(2));
    let mut state = InlineState::default();
    state.control.session_mut().compaction_mut().active = Some(active);

    // A newer recommendation (revision B = generation 1, revision 3) is pending
    // while the compactor for revision A is still running.
    note_compaction_recommendation(
        &mut state,
        "00000000-0000-4000-8000-000000000000:1:3:200000:100000",
    );
    assert!(state.control.session().compaction().has_pending_auto());

    // One cancel stops both: revision B is suppressed now, revision A's
    // compactor is terminated.
    let mut cancel_output = Vec::new();
    cancel_compaction(&mut state, &mut cancel_output).expect("cancel");
    assert!(String::from_utf8(cancel_output)
        .expect("UTF-8")
        .contains("Cancellation requested"));
    assert!(!state.control.session().compaction().has_pending_auto());

    // The active compactor reports its cancelled completion; the poll harvests
    // it, adds revision A to the suppressed set, and renders the notice.
    sender
        .send(CompactionOutcome::Failed {
            code: "transport".to_string(),
            message: "terminated".to_string(),
        })
        .expect("queue cancelled outcome");
    let adapter = AdapterInstance::Fake(FakeAgentAdapter);
    let mut poll_output = Vec::new();
    poll_background_compaction(&mut state, &mut poll_output, &adapter, false).expect("poll");
    assert!(String::from_utf8(poll_output)
        .expect("UTF-8")
        .contains("cancelled"));
    assert!(!compaction_active(&state));

    // Re-emitting the cancelled PENDING revision B must NOT re-trigger: A's
    // completion did not clobber B's suppression marker.
    note_compaction_recommendation(
        &mut state,
        "00000000-0000-4000-8000-000000000000:1:3:200000:100000",
    );
    assert!(
        !state.control.session().compaction().has_pending_auto(),
        "cancelled pending revision retriggered compaction"
    );

    // The active revision A stays suppressed too.
    assert!(state
        .control
        .session()
        .compaction()
        .is_auto_marker_suppressed(&marker(2)));
    assert!(state
        .control
        .session()
        .compaction()
        .is_auto_marker_suppressed(&marker(3)));

    // A genuinely new projection revision is a distinct identity and still
    // triggers normally.
    note_compaction_recommendation(
        &mut state,
        "00000000-0000-4000-8000-000000000000:1:4:200000:100000",
    );
    assert!(state.control.session().compaction().has_pending_auto());
}
