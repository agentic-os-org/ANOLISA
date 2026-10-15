use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::slice::{
    approval_card_view, approval_slice_eligibility, focused_action, ApprovalActionKey,
    SliceRejectReason,
};
use crate::adapter::ApprovalChannelMessage;
use crate::agent::run::test_support::test_active_run_with_id;
use crate::approval::slice_present::relinquish_untrusted_and_render_inline;
use crate::i18n::I18n;
use crate::raw_input::{update_input_mode, RawInputCapture, RawInputMode, RawObserverAction};
use crate::runtime::approval_ui::ApprovalSliceActivation;
use crate::runtime::prelude::{
    AgentEvent, AgentRunHandle, AgentRunOrigin, ApprovalActionSet, ApprovalPanelAction,
    ApprovalRequestKind, ApprovalRequestStatus, GovernanceDecision, GovernancePolicyDecision,
    GovernedEvent, InlineState, Language, ProviderShellRequestKind, QuestionSelectionMode,
    RuntimeApprovalRequest, RuntimeCommandAssessmentSummary,
};
use crate::shell_host::{retire_capture_for_representation, CaptureRepresentationReset};
use crate::ui::worker_protocol::{ApprovalActionView, UiLanguage};
use crate::ui::QuestionInputFeedback;
use crate::MessageId;

#[path = "slice_tests/live_eligibility.rs"]
mod live_eligibility;

const RUN_ID: &str = "run-1";
const REQUEST_ID: &str = "ctrl-1";
const TOOL_USE_ID: &str = "toolu-1";
const APPROVAL_ID: &str = "req-1";

fn activation() -> ApprovalSliceActivation {
    ApprovalSliceActivation {
        worker_path: "/opt/cosh/bin/cosh-ui-worker".into(),
        limits: crate::ui::worker_protocol::ProtocolLimits {
            max_payload_bytes: 1_048_576,
            max_draw_ops: 4096,
            max_text_bytes: 16_384,
        },
        max_surface_rows: 24,
        columns: 40,
        handshake_timeout: Duration::from_millis(5000),
        render_timeout: Duration::from_millis(5000),
        heartbeat_interval: Duration::from_millis(100),
        heartbeat_timeout: Duration::from_millis(1000),
        backpressure_timeout: Duration::from_millis(5000),
        shutdown_timeout: Duration::from_millis(2000),
        failure_limit: 3,
    }
}

/// A request that satisfies every rule: a pending provider control permission
/// for an executable Bash tool, classified high risk so its action set is the
/// three-action high-risk set.
fn eligible_request() -> RuntimeApprovalRequest {
    RuntimeApprovalRequest {
        id: APPROVAL_ID.to_string(),
        audit_ref: None,
        run_id: RUN_ID.to_string(),
        origin: AgentRunOrigin::Standard,
        session_id: "sess-1".to_string(),
        cwd: "/tmp".to_string(),
        source: "control-protocol",
        provider_shell_request_kind: ProviderShellRequestKind::ControlPermission,
        kind: ApprovalRequestKind::Tool,
        subject: "Bash".to_string(),
        preview: "$ printf '%s\\n' slice_effect >> '/tmp/effect.txt'".to_string(),
        risk: "high",
        request_id: Some(REQUEST_ID.to_string()),
        tool_use_id: Some(TOOL_USE_ID.to_string()),
        tool_input: Some(serde_json::json!({ "command": "printf slice_effect" })),
        original_user_request: None,
        status: ApprovalRequestStatus::Pending,
        execution_path: Some("provider_control_protocol"),
        command_block_id: None,
        redaction_status: None,
        assessment: Some(RuntimeCommandAssessmentSummary {
            impact: "high",
            execution: "host",
            confidence: "high",
            primary_reason: "filesystem-write",
            reason_trace: "redirection,filesystem-write".to_string(),
            auto_allow: None,
            output_stability: "stable",
            output_exposure: "low",
        }),
        hook_requires_approval: false,
        hook_warnings: Vec::new(),
    }
}

fn governed_permission(run_id: &str, request_id: &str) -> GovernedEvent {
    GovernedEvent {
        policy_decision: GovernancePolicyDecision::NeedsUserApproval,
        decision: GovernanceDecision::Display,
        display_text: "approval required".to_string(),
        reason: "needs user approval".to_string(),
        auto_execute: false,
        event: AgentEvent::ToolPermissionRequest {
            run_id: run_id.to_string(),
            request_id: request_id.to_string(),
            tool_name: "Bash".to_string(),
            tool_input: serde_json::json!({ "command": "printf slice_effect" }),
            tool_use_id: TOOL_USE_ID.to_string(),
            hook_requires_approval: false,
            audit_ref: None,
        },
    }
}

/// An `InlineState` whose active run owns the request and whose provider has
/// announced it can accept a host-executed shell tool result.
fn owning_state(
    request: &RuntimeApprovalRequest,
) -> (InlineState, Receiver<ApprovalChannelMessage>) {
    capable_state_with(request, true, RUN_ID, REQUEST_ID)
}

fn capable_state_with(
    request: &RuntimeApprovalRequest,
    capable: bool,
    run_id: &str,
    owned_request_id: &str,
) -> (InlineState, Receiver<ApprovalChannelMessage>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let (mut run, _default_receiver) = test_active_run_with_id(run_id);
    run.handle = if capable {
        AgentRunHandle::test_with_host_executed_shell_result(sender)
    } else {
        AgentRunHandle::test_with_approval_sender(sender)
    };
    run.governed_events
        .push(governed_permission(run_id, owned_request_id));
    let mut state = InlineState::default();
    state.agent_run.active = Some(run);
    state.approvals.requests.push(request.clone());
    (state, receiver)
}

fn eligible() -> (
    ApprovalSliceActivation,
    InlineState,
    RuntimeApprovalRequest,
    Receiver<ApprovalChannelMessage>,
) {
    let request = eligible_request();
    let (state, receiver) = owning_state(&request);
    (activation(), state, request, receiver)
}

#[test]
fn an_eligible_high_risk_bash_control_permission_is_accepted() {
    let (activation, state, request, _receiver) = eligible();

    let slice = approval_slice_eligibility(Some(&activation), &state, &request, 7)
        .expect("every rule holds, so the slice must be taken");

    assert_eq!(
        slice.action_key,
        ApprovalActionKey {
            shell_session_id: "sess-1".to_string(),
            run_id: RUN_ID.to_string(),
            approval_id: APPROVAL_ID.to_string(),
            request_id: REQUEST_ID.to_string(),
            tool_use_id: TOOL_USE_ID.to_string(),
        }
    );
    assert_eq!(slice.card.revision, 7);
    assert_eq!(slice.card.actions.len(), 3);
    assert_eq!(slice.card.selected, ApprovalActionView::Approve);
}

/// The projection must not carry anything that could decide or dispatch the
/// approval: no tool input, no provider identifiers, no callbacks.
#[test]
fn the_projection_carries_no_decision_or_dispatch_material() {
    let (activation, state, request, _receiver) = eligible();
    let slice =
        approval_slice_eligibility(Some(&activation), &state, &request, 1).expect("eligible");

    let wire = serde_json::to_string(&slice.card).expect("card serializes");
    for forbidden in [
        TOOL_USE_ID,
        REQUEST_ID,
        "tool_input",
        "printf slice_effect\"]",
    ] {
        assert!(
            !wire.contains(forbidden),
            "{forbidden:?} must not cross to the worker: {wire}"
        );
    }
    // The preview is the already-redacted string the inline card shows.
    assert_eq!(slice.card.preview, request.preview);
    assert_eq!(slice.card.display_id, APPROVAL_ID);
}

/// One case per rule. Each mutates exactly one fact of an otherwise eligible
/// fixture, so deleting any single guard turns exactly one case red.
#[test]
fn every_reject_reason_is_reachable_on_its_own() {
    let cases: Vec<(SliceRejectReason, fn(&mut RuntimeApprovalRequest))> = vec![
        (SliceRejectReason::NotPending, |request| {
            request.status = ApprovalRequestStatus::Approved
        }),
        (SliceRejectReason::HookRequest, |request| {
            request.hook_requires_approval = true
        }),
        (SliceRejectReason::TurnExtensionRequest, |request| {
            request.kind = ApprovalRequestKind::TurnExtension
        }),
        (SliceRejectReason::NotExecutableBashTool, |request| {
            request.subject = "Read".to_string()
        }),
        (SliceRejectReason::NotControlPermission, |request| {
            request.provider_shell_request_kind = ProviderShellRequestKind::LocalApproval
        }),
        (SliceRejectReason::MissingProviderIds, |request| {
            request.request_id = None
        }),
        (SliceRejectReason::NotStandardHighRiskActionSet, |request| {
            request.risk = "medium"
        }),
    ];
    for (expected, mutate) in cases {
        let (activation, state, mut request, _receiver) = eligible();
        mutate(&mut request);
        assert_eq!(
            approval_slice_eligibility(Some(&activation), &state, &request, 1),
            Err(expected),
            "mutating one fact must yield {expected:?}"
        );
    }
}

/// `SliceRejectReason` has no `Debug`-free path, so equality is asserted through
/// `Result`; these three need state rather than request mutations.
#[test]
fn the_state_dependent_reject_reasons_are_reachable() {
    let activation = activation();
    let request = eligible_request();

    // No activation at all.
    let (state, _receiver) = owning_state(&request);
    assert_eq!(
        approval_slice_eligibility(None, &state, &request, 1),
        Err(SliceRejectReason::NotActivated)
    );

    // No active run owning the request.
    let mut orphan = InlineState::default();
    orphan.approvals.requests.push(request.clone());
    assert_eq!(
        approval_slice_eligibility(Some(&activation), &orphan, &request, 1),
        Err(SliceRejectReason::NotOwnedByActiveRun)
    );

    // An active run that saw a different provider request id.
    let (foreign, _foreign_rx) = capable_state_with(&request, true, RUN_ID, "ctrl-other");
    assert_eq!(
        approval_slice_eligibility(Some(&activation), &foreign, &request, 1),
        Err(SliceRejectReason::NotOwnedByActiveRun)
    );

    // The owning provider never announced host-executed shell results, so the
    // handoff the slice would promise could not be delivered.
    let (incapable, _incapable_rx) = capable_state_with(&request, false, RUN_ID, REQUEST_ID);
    assert_eq!(
        approval_slice_eligibility(Some(&activation), &incapable, &request, 1),
        Err(SliceRejectReason::ProviderCannotReceiveHostResult)
    );

    // A second pending approval makes ownership of the worker ambiguous.
    let (mut crowded, _crowded_rx) = owning_state(&request);
    let mut second = eligible_request();
    second.id = "req-2".to_string();
    crowded.approvals.requests.push(second);
    assert_eq!(
        approval_slice_eligibility(Some(&activation), &crowded, &request, 1),
        Err(SliceRejectReason::NotSolePendingApproval)
    );
}

/// A second approval in the same run raises the same-run count, which upgrades
/// the action set to turn consent; the slice must not present a card whose
/// actions it cannot honour.
#[test]
fn a_same_run_history_that_upgrades_the_action_set_is_rejected() {
    let activation = activation();
    let request = eligible_request();
    let (mut state, _receiver) = owning_state(&request);
    let mut resolved = eligible_request();
    resolved.id = "req-0".to_string();
    resolved.status = ApprovalRequestStatus::Approved;
    // Inserted first so the eligible request is still the only pending one.
    state.approvals.requests.insert(0, resolved);

    assert_eq!(
        approval_action_set_for_public(&state, &request),
        ApprovalActionSet::TurnConsentHighRisk
    );
    assert_eq!(
        approval_slice_eligibility(Some(&activation), &state, &request, 1),
        Err(SliceRejectReason::NotStandardHighRiskActionSet)
    );
}

fn approval_action_set_for_public(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> ApprovalActionSet {
    crate::approval::panel::approval_action_set_for(request, &state.approvals.requests)
}

/// A higher-priority card owner holds input capture, so the worker would paint a
/// card whose keystrokes go somewhere else.
#[test]
fn a_higher_priority_card_owner_rejects_the_slice() {
    let (activation, mut state, request, _receiver) = eligible();
    assert!(
        approval_slice_eligibility(Some(&activation), &state, &request, 1).is_ok(),
        "the fixture must be eligible before a question is added"
    );
    state.questions.pending_id = Some("question-1".to_string());
    state
        .questions
        .items
        .push(crate::question::runtime::RuntimeUserQuestion {
            id: "question-1".to_string(),
            question: "pick one".to_string(),
            options: vec!["a".to_string(), "b".to_string()],
            selected_option: 0,
            selected_options: Vec::new(),
            custom_answer: String::new(),
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::default(),
            provider_request_id: None,
            provider_owner_request_id: None,
            origin: AgentRunOrigin::Standard,
            answer: None,
        });

    assert_eq!(
        approval_slice_eligibility(Some(&activation), &state, &request, 1),
        Err(SliceRejectReason::HigherPriorityCardOwner)
    );
}

/// Condition 8 of the design is a tripwire, not a reachable branch: once rules 3
/// and 4 hold, an approved shell-tool control permission always classifies as a
/// foreground handoff. What is testable is the premise it rests on, so a future
/// Condition 8 of the design is a tripwire, not a reachable branch: once the
/// request is known to be an executable Bash tool, an approved outcome always
/// classifies as a foreground handoff. What is testable is the premise it rests
/// on, so a future change to the classifier fails here instead of silently
/// widening the slice.
#[test]
fn the_handoff_tripwire_premise_holds() {
    use crate::approval::broker::{
        classify_approval_outcome, ApprovalOutcome, ApprovalOutcomeInput,
    };
    let classify = |shell_tool, provider_request| {
        classify_approval_outcome(ApprovalOutcomeInput {
            approved: true,
            shell_tool,
            provider_request,
        })
    };
    assert_eq!(
        classify(true, true),
        ApprovalOutcome::ForegroundShellHandoff,
        "the eligible shape must classify as a handoff"
    );
    // `provider_request` is consulted only on the not-approved and not-shell
    // paths, so it cannot move an approved shell-tool outcome. That is what makes
    // the tripwire unreachable by construction rather than merely untested.
    assert_eq!(
        classify(true, false),
        ApprovalOutcome::ForegroundShellHandoff
    );
    for provider_request in [false, true] {
        assert_ne!(
            classify(false, provider_request),
            ApprovalOutcome::ForegroundShellHandoff,
            "a non-shell tool must not classify as a handoff"
        );
    }
}

#[test]
fn focus_falls_back_to_approve_when_the_recorded_action_left_the_set() {
    let (_activation, mut state, request, _receiver) = eligible();
    state
        .approvals
        .focus
        .insert(APPROVAL_ID.to_string(), ApprovalPanelAction::AlwaysTrust);
    assert_eq!(
        focused_action(&state, &request),
        ApprovalPanelAction::Approve
    );

    state
        .approvals
        .focus
        .insert(APPROVAL_ID.to_string(), ApprovalPanelAction::Deny);
    assert_eq!(focused_action(&state, &request), ApprovalPanelAction::Deny);

    let card = approval_card_view(3, &state, &request, ApprovalPanelAction::Deny, false)
        .expect("high-risk set is presentable");
    assert_eq!(card.selected, ApprovalActionView::Deny);
    assert_eq!(card.language, UiLanguage::En);
    assert_eq!(
        card.actions,
        vec![
            ApprovalActionView::Approve,
            ApprovalActionView::Deny,
            ApprovalActionView::Details,
        ]
    );
    assert!(!card.irrecoverable);
}

#[test]
fn a_system_control_assessment_marks_the_card_irrecoverable() {
    let (state, request) = {
        let mut request = eligible_request();
        request
            .assessment
            .as_mut()
            .expect("assessment")
            .reason_trace = "redirection,system-control".to_string();
        let (state, _receiver) = owning_state(&request);
        (state, request)
    };
    let card = approval_card_view(1, &state, &request, ApprovalPanelAction::Approve, false)
        .expect("presentable");
    assert!(card.irrecoverable);
}

#[test]
fn a_chinese_session_projects_chinese_labels() {
    let (activation, mut state, request, _receiver) = eligible();
    state.language = Language::ZhCn;
    let slice =
        approval_slice_eligibility(Some(&activation), &state, &request, 1).expect("eligible");
    assert_eq!(slice.card.language, UiLanguage::Zh);
}

/// The action set the slice can present is the three-action high-risk set; the
/// projection must refuse anything wider rather than dropping actions.
#[test]
fn the_projection_refuses_an_action_set_it_cannot_present() {
    let (_activation, mut state, mut request, _receiver) = eligible();
    request.risk = "medium";
    state.approvals.requests[0] = request.clone();
    assert_eq!(
        approval_card_view(1, &state, &request, ApprovalPanelAction::Approve, false),
        None
    );
}

fn approval_capture() -> RawInputCapture {
    RawInputCapture::Approval {
        id: "req-1".to_owned(),
        action_set: ApprovalActionSet::TurnConsentHighRisk,
    }
}

fn live_capture_generation(input_mode: &Arc<Mutex<RawInputMode>>) -> u64 {
    match &*input_mode.lock().expect("input mode lock") {
        RawInputMode::Capture { generation, .. } => *generation,
        other => panic!("expected a live capture, got {other:?}"),
    }
}

fn install_approval_capture() -> Arc<Mutex<RawInputMode>> {
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    update_input_mode(
        &input_mode,
        &RawObserverAction::CaptureInput(approval_capture()),
        None,
    );
    input_mode
}

/// A resize invalidates the worker surface and the same approval is rebuilt as an
/// inline card. Keystrokes read while the old presentation was on screen belong to
/// it, and the relay already rejects a batch whose stamped generation is not the
/// current one (`raw_input/spawn/capture.rs:75-91`). That protection is worth
/// nothing unless the rebuild retires the generation, so the same approval
/// re-presented must come back under a new one.
#[test]
fn re_presenting_a_pending_approval_retires_its_capture_generation() {
    let input_mode = install_approval_capture();
    let installed = live_capture_generation(&input_mode);

    // A focus repaint re-issues the identical capture and must keep the generation,
    // otherwise every arrow key would orphan the keystrokes already in flight.
    update_input_mode(
        &input_mode,
        &RawObserverAction::CaptureInput(approval_capture()),
        None,
    );
    assert_eq!(
        live_capture_generation(&input_mode),
        installed,
        "re-issuing the same capture must not re-mint on its own"
    );

    assert!(
        retire_capture_for_representation(&mut input_mode.lock().expect("input mode lock")),
        "a live approval capture must be retired for re-presentation"
    );

    update_input_mode(
        &input_mode,
        &RawObserverAction::CaptureInput(approval_capture()),
        None,
    );
    assert_ne!(
        live_capture_generation(&input_mode),
        installed,
        "the retired generation must not be reused for the replacement card"
    );
}

/// The retirement must not park a still-Pending approval on an owner that replays
/// input into the shell. `Hold` reports held input and returns without writing to
/// the PTY (`raw_input/spawn/capture.rs:172-176`), which is exactly what
/// `abandon_active_capture` cannot offer: its `post_owner` is `MainPrompt`
/// (`raw_input/mode.rs:324`), and it leaves the chain in `Draining`, where a later
/// `CaptureInput` for the same capture is ignored (`raw_input/mode.rs:86`) so the
/// replacement card would receive no input at all.
#[test]
fn retiring_a_capture_for_re_presentation_holds_instead_of_releasing_to_the_shell() {
    let input_mode = install_approval_capture();
    retire_capture_for_representation(&mut input_mode.lock().expect("input mode lock"));
    assert_eq!(
        *input_mode.lock().expect("input mode lock"),
        RawInputMode::Hold,
        "input must be held, never handed to the shell while the approval is Pending"
    );
}

/// A retirement requested while no capture is live must change nothing: it must not
/// invent a capture, and it must not move an owner the shell currently relies on.
#[test]
fn retiring_with_no_live_capture_changes_nothing() {
    let input_mode = Arc::new(Mutex::new(RawInputMode::Passthrough));
    assert!(
        !retire_capture_for_representation(&mut input_mode.lock().expect("input mode lock")),
        "there is no capture to retire"
    );
    assert_eq!(
        *input_mode.lock().expect("input mode lock"),
        RawInputMode::Passthrough
    );
}

/// The relay consumes the reset between the observer pass and the mode update, so
/// the sequence it performs is itself a contract: take the flag, retire the live
/// capture, then apply the observer's re-issued `CaptureInput` for the same card.
/// Pinning the sequence is what ties the handle to the cutover — separately the two
/// halves are each correct and never meet.
#[test]
fn the_relay_reset_sequence_mints_a_new_generation_for_the_same_approval() {
    let reset = CaptureRepresentationReset::default();
    let input_mode = install_approval_capture();
    let installed = live_capture_generation(&input_mode);

    reset.signal();
    assert!(reset.consume(), "a signalled reset must be consumable");
    assert!(!reset.consume(), "consuming must clear the request");

    assert!(retire_capture_for_representation(
        &mut input_mode.lock().expect("input mode lock")
    ));
    update_input_mode(
        &input_mode,
        &RawObserverAction::CaptureInput(approval_capture()),
        None,
    );

    assert_ne!(
        live_capture_generation(&input_mode),
        installed,
        "the relay's own sequence must leave the same card on a new generation"
    );
}

/// The fallback is the only thing that asks for a cutover. Without this link the
/// mode contract and the relay sequence are both correct and the product still
/// keeps the old generation, because nothing ever signals the reset.
#[test]
fn relinquishing_an_untrusted_surface_signals_a_representation_reset() {
    let (_activation, mut state, _request, _receiver) = eligible();
    state.approval_slice.session = None;
    let mut output = Vec::new();

    relinquish_untrusted_and_render_inline(&mut state, &mut output)
        .expect("rendering into a buffer cannot fail");

    assert!(
        state.approval_slice.capture_representation_reset.consume(),
        "rebuilding the card inline must retire the generation it was presented under"
    );
}

/// The rebuilt card has to say why the display above it changed, in the session's
/// own language, and as product state rather than as a process or mechanism.
#[test]
fn relinquishing_an_untrusted_surface_announces_the_rebuilt_card_in_both_locales() {
    for language in [Language::EnUs, Language::ZhCn] {
        let (_activation, mut state, _request, _receiver) = eligible();
        state.language = language;
        state.approval_slice.session = None;
        let mut output = Vec::new();

        relinquish_untrusted_and_render_inline(&mut state, &mut output).expect("renders");

        let rendered = String::from_utf8(output).expect("the card is valid utf-8");
        let notice = I18n::new(language).t(MessageId::ApprovalPresentationInvalidatedNotice);
        assert!(!notice.is_empty(), "the notice must exist in this locale");
        assert!(
            rendered.contains(notice),
            "the {language:?} fallback must announce the rebuilt card, got:\n{rendered}"
        );
    }
}

#[test]
fn failed_inline_replacement_still_requests_capture_retirement() {
    struct FailedOutput {
        remaining: usize,
        fail_flush: bool,
    }
    impl std::io::Write for FailedOutput {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.remaining == 0 {
                return Err(std::io::Error::other("replacement write failed"));
            }
            let written = bytes.len().min(self.remaining);
            self.remaining -= written;
            Ok(written)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.fail_flush {
                Err(std::io::Error::other("replacement flush failed"))
            } else {
                Ok(())
            }
        }
    }
    for (remaining, fail_flush) in [(0, false), (24, false), (usize::MAX, true)] {
        let (_activation, mut state, _request, _receiver) = eligible();
        let result = relinquish_untrusted_and_render_inline(
            &mut state,
            &mut FailedOutput {
                remaining,
                fail_flush,
            },
        );
        assert!(result.is_err());
        assert!(
            state.approval_slice.capture_representation_reset.consume(),
            "an invisible replacement must retire the old capture"
        );
        assert!(state.approvals.active_panel_id.is_none());
        assert_eq!(
            state.approvals.requests[0].status,
            ApprovalRequestStatus::Pending
        );
        if !fail_flush {
            let (activation, mut state, request, _receiver) = eligible();
            state.approval_slice.activation = Some(activation);
            state.approval_slice.observer_event_end = 4097;
            state.approvals.active_panel_id = Some(request.id);
            state.approvals.active_panel_height = 4;
            assert!(crate::approval::panel::clear_active_approval_panel(
                &mut state,
                &mut FailedOutput {
                    remaining,
                    fail_flush
                },
            )
            .is_err());
            assert!(state.approval_slice.capture_representation_reset.consume());
            assert_eq!(state.approval_slice.invalidated_card_events_before, 4097);
            assert!(state.approvals.active_panel_id.is_none());
            assert_eq!(state.approvals.active_panel_height, 0);
            assert_eq!(
                state.approvals.requests[0].status,
                ApprovalRequestStatus::Pending
            );
        }
    }
}

#[test]
fn a_rebuilt_inline_card_invalidates_on_an_unreadable_viewport() {
    let (activation, mut state, request, _receiver) = eligible();
    state.approval_slice.activation = Some(activation);
    state.approval_slice.disabled = true;
    state.approval_slice.inline_geometry = Some((30, 80));
    state.approvals.active_panel_id = Some(request.id);
    state.approvals.active_panel_height = 12;
    let mut output = Vec::new();
    let result = crate::approval::slice_present::repaint_worker_slice_if_viewport_changed(
        &mut state,
        &mut output,
    );
    assert!(
        result.is_err(),
        "an unreadable viewport cannot publish a fresh card"
    );
    assert!(state.approval_slice.capture_representation_reset.consume());
    assert!(state.approvals.active_panel_id.is_none());
    assert_eq!(state.approvals.active_panel_height, 0);
    assert!(!output.windows(4).any(|bytes| bytes == b"\x1b[2K"));
    struct ViewportReset(Option<(u16, u16)>);
    impl Drop for ViewportReset {
        fn drop(&mut self) {
            crate::approval::slice_present::set_test_viewport(self.0);
        }
    }
    struct ResizeOnFlush {
        bytes: Vec<u8>,
        resize_on_scroll: bool,
        duplicate_flush_kind: u8,
        flushed_bytes: usize,
    }
    impl std::io::Write for ResizeOnFlush {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.resize_on_scroll && bytes.first() == Some(&0x18) {
                crate::approval::slice_present::set_test_viewport(Some((96, 47)));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            if self.duplicate_flush_kind != 0
                && !self.bytes.is_empty()
                && self.flushed_bytes == self.bytes.len()
            {
                if self.duplicate_flush_kind == 2 {
                    return Err(std::io::Error::other("post-publication flush failed"));
                }
                crate::approval::slice_present::set_test_viewport(Some((96, 47)));
                return Ok(());
            }
            self.flushed_bytes = self.bytes.len();
            if !self.resize_on_scroll || !self.bytes.contains(&0x18) {
                crate::approval::slice_present::set_test_viewport(Some((48, 47)));
            }
            Ok(())
        }
    }
    let _restore = ViewportReset(crate::approval::slice_present::set_test_viewport(Some((
        24, 47,
    ))));
    for (resize_on_scroll, duplicate_flush_kind) in [(false, 0), (true, 0), (false, 2), (false, 1)]
    {
        crate::approval::slice_present::set_test_viewport(Some((24, 47)));
        let (activation, mut state, request, _receiver) = eligible();
        state.approval_slice.activation = Some(activation);
        state.approval_slice.disabled = true;
        state.approval_slice.inline_geometry = Some((24, 47));
        state.approvals.active_panel_id = Some(request.id);
        state.approvals.active_panel_height = 8;
        let mut output = ResizeOnFlush {
            bytes: Vec::new(),
            resize_on_scroll,
            duplicate_flush_kind,
            flushed_bytes: 0,
        };
        let result = relinquish_untrusted_and_render_inline(&mut state, &mut output);
        if resize_on_scroll {
            assert!(
                result.is_err(),
                "a viewport change during scrollout cannot grant a fresh card"
            );
            assert!(state.approvals.active_panel_id.is_none());
        } else if duplicate_flush_kind == 2 && result.is_err() {
            assert!(
                state.approvals.active_panel_id.is_none(),
                "failed post-publication IO must revoke the claim"
            );
        } else {
            result.expect("stable viewport after flush must publish");
            assert!(
                output.bytes.starts_with(b"\x18\x1b[48;1H"),
                "scrollout must sample geometry after flush"
            );
            assert_eq!(
                state.approval_slice.inline_geometry,
                Some(crate::approval::slice_present::current_viewport().unwrap())
            );
        }
    }
    crate::approval::slice_present::set_test_viewport(Some((24, 39)));
    let (activation, mut state, _request, _receiver) = eligible();
    state.approval_slice.activation = Some(activation);
    state.approval_slice.disabled = true;
    let mut output = Vec::new();
    relinquish_untrusted_and_render_inline(&mut state, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    let card_height = state.approvals.active_panel_height;
    assert!(card_height > 2);
    let card_start = output
        .lines()
        .count()
        .checked_sub(card_height)
        .expect("published card rows must be present in output");
    let mut card = output.lines().skip(card_start);
    assert!(card
        .all(|line| { crate::ui::agent_render::display_width(line.trim_end_matches('\r')) <= 39 }));
}

#[test]
fn rebuilding_preserves_observed_focus_before_fencing_the_old_events() {
    let (_activation, mut state, request, _receiver) = eligible();
    state.approval_slice.observed_focus = Some((request.id.clone(), ApprovalPanelAction::Deny));
    state.approval_slice.observer_event_end = 4097;
    relinquish_untrusted_and_render_inline(&mut state, &mut Vec::new()).expect("rebuild");
    assert_eq!(
        state.approvals.focus.get(&request.id),
        Some(&ApprovalPanelAction::Deny)
    );
    assert_eq!(state.approval_slice.invalidated_card_events_before, 4097);
    assert_eq!(
        state.approvals.requests[0].status,
        ApprovalRequestStatus::Pending
    );
}

fn live_idle_closed_tty_state() -> (InlineState, Receiver<ApprovalChannelMessage>) {
    let (activation, mut state, request, receiver) = eligible();
    let action_key = approval_slice_eligibility(Some(&activation), &state, &request, 1)
        .expect("eligible fixture")
        .action_key;
    let mut session =
        crate::approval::surface_session::TtyApprovalSurfaceSession::idle_closed_tty_for_test(
            activation.clone(),
            action_key,
        );
    session
        .pump()
        .expect("model idle session has no frame to publish");
    state.approval_slice.activation = Some(activation);
    state.approval_slice.session = Some(Box::new(session));
    state.approval_slice.observer_event_end = 4097;
    state.approvals.active_panel_id = Some(request.id);
    state.approvals.active_panel_height = 0;
    (state, receiver)
}

fn assert_live_io_failure_retires_claim(state: &mut InlineState, status: ApprovalRequestStatus) {
    assert!(state.approval_slice.capture_representation_reset.consume());
    assert_eq!(state.approval_slice.invalidated_card_events_before, 4097);
    assert!(state.approval_slice.session.is_none());
    assert!(state.approval_slice.disabled);
    assert!(state.approvals.active_panel_id.is_none());
    assert_eq!(state.approvals.active_panel_height, 0);
    assert_eq!(state.approvals.requests[0].status, status);
    assert!(state.approvals.journal.is_empty());
}

#[test]
fn slice_live_io_failure_on_flush_retires_idle_capture() {
    struct FailedFlush;
    impl std::io::Write for FailedFlush {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("idle flush failed"))
        }
    }
    let (mut state, _receiver) = live_idle_closed_tty_state();
    crate::approval::slice_present::pump_worker_slice(&mut state, &mut Vec::new())
        .expect("a successful idle flush keeps the existing capture");
    assert!(state
        .approval_slice
        .session
        .as_ref()
        .is_some_and(|session| session.is_live()));
    assert!(state.approvals.active_panel_id.is_some());
    assert!(!state.approval_slice.capture_representation_reset.consume());
    let mut failures = Vec::new();
    for path in ["pump", "release", "rebuild", "idle", "repaint", "present"] {
        let (mut state, _receiver) = live_idle_closed_tty_state();
        if path == "idle" {
            state.approvals.requests[0].status = ApprovalRequestStatus::Denied;
        }
        if path == "present" {
            state.approvals.active_panel_id = None;
        }
        let result = match path {
            "pump" => {
                crate::approval::slice_present::pump_worker_slice(&mut state, &mut FailedFlush)
            }
            "release" => {
                crate::approval::slice_present::release_worker_slice(&mut state, &mut FailedFlush)
                    .map(|_| ())
            }
            "rebuild" => relinquish_untrusted_and_render_inline(&mut state, &mut FailedFlush),
            "idle" => crate::approval::slice_present::present_via_worker_slice(
                &mut state,
                &mut FailedFlush,
            )
            .map(|_| ()),
            "present" => crate::approval::slice_present::present_via_worker_slice(
                &mut state,
                &mut FailedFlush,
            )
            .map(|_| ()),
            "repaint" => {
                crate::approval::slice_present::repaint_worker_slice(&mut state, &mut FailedFlush)
                    .map(|_| ())
            }
            _ => unreachable!(),
        };
        match result {
            Err(error) if error.to_string() == "idle flush failed" => {
                let status = if path == "idle" {
                    ApprovalRequestStatus::Denied
                } else {
                    ApprovalRequestStatus::Pending
                };
                assert_live_io_failure_retires_claim(&mut state, status);
            }
            result => failures.push(format!(
                "{path}: original flush error not preserved: {result:?}"
            )),
        }
    }
    assert!(failures.is_empty(), "{failures:?}");
}

#[test]
fn slice_live_io_failure_on_withdraw_retires_capture() {
    let (mut state, _receiver) = live_idle_closed_tty_state();
    assert!(
        crate::approval::slice_present::release_worker_slice(&mut state, &mut Vec::new()).is_err()
    );
    assert_live_io_failure_retires_claim(&mut state, ApprovalRequestStatus::Pending);
}
