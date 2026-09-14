use crate::runtime::prelude::*;
use crate::tools::known_provider_tool;

/// Irrecoverable warning trigger (#2064): anchored on the classifier
/// verdict (`system-control` in the assessment reason trace), never on
/// command-name matching in the render layer. Trims each code so a
/// future separator change in `reason_trace` cannot silently disable
/// the warning. Wiring pinned end-to-end by
/// `classifier_verdict_wires_irrecoverable_panel_flag`.
pub(crate) fn assessment_is_irrecoverable(assessment: &RuntimeCommandAssessmentSummary) -> bool {
    assessment
        .reason_trace
        .split(',')
        .any(|reason| reason.trim() == "system-control")
}

/// Single source of truth for which action list an approval request offers
/// (issue #1773): hook requests keep the hook list; a request whose run
/// already has ≥ 2 approval requests (queued or resolved) offers turn-scope
/// batch consent; everything else keeps the standard list.
///
/// Counting invariant: the requests vec is append-only — the
/// `approval/runtime.rs` request lifecycle only appends entries and flips
/// their status, never removes them — so the same-run count is monotonic
/// within a run. Pinned by `approval_action_set_matrix`.
pub(crate) fn approval_action_set_for(
    request: &RuntimeApprovalRequest,
    requests: &[RuntimeApprovalRequest],
) -> ApprovalActionSet {
    if request.kind == ApprovalRequestKind::TurnExtension {
        return ApprovalActionSet::TurnExtension;
    }
    if request.subject.starts_with("HOOK:") {
        return ApprovalActionSet::Hook;
    }
    // High-risk requests never offer AlwaysTrust (issue #2064): an
    // irrecoverable or otherwise high-impact command must be explicitly
    // approved on every dispatch. Same predicate family as
    // `batch_consent_covers_request` so the two consent gates can't drift.
    let high_risk = request.risk == "high";
    let same_run = requests
        .iter()
        .filter(|other| other.run_id == request.run_id)
        .count();
    match (same_run >= 2, high_risk) {
        (true, true) => ApprovalActionSet::TurnConsentHighRisk,
        (true, false) => ApprovalActionSet::TurnConsent,
        (false, true) => ApprovalActionSet::StandardHighRisk,
        (false, false) => ApprovalActionSet::Standard,
    }
}

pub(crate) fn render_approval_requests<W: Write>(
    state: &mut InlineState,
    approval_ids: &[String],
    output: &mut W,
) -> std::io::Result<()> {
    if approval_ids.is_empty() {
        return Ok(());
    }

    render_current_approval_request(state, output)
}

pub(crate) fn render_current_approval_request<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some((index, request)) = state
        .approvals
        .requests
        .iter()
        .enumerate()
        .find(|(_, request)| request.status == ApprovalRequestStatus::Pending)
    else {
        return Ok(());
    };

    if state.approvals.active_panel_id.as_deref() == Some(request.id.as_str()) {
        return Ok(());
    }

    // The agent status spinner repaints in place (`\r\x1b[2K<frame> ...`) and
    // leaves the cursor mid-line; clear it before drawing the panel so the
    // card border starts at column 0 instead of wrapping past the row end.
    if let Some(active_run) = state.agent_run.active.as_mut() {
        active_run.status_animation.clear(output)?;
    }

    let pending_total = state
        .approvals
        .requests
        .iter()
        .filter(|request| request.status == ApprovalRequestStatus::Pending)
        .count();
    let pending_position = state
        .approvals
        .requests
        .iter()
        .take(index + 1)
        .filter(|request| request.status == ApprovalRequestStatus::Pending)
        .count();
    let next_pending = state
        .approvals
        .requests
        .iter()
        .skip(index + 1)
        .find(|request| request.status == ApprovalRequestStatus::Pending);
    let i18n = state.i18n();
    let preview_label = match request.kind {
        ApprovalRequestKind::Tool => i18n.t(MessageId::ApprovalToolInputLabel),
        ApprovalRequestKind::ShellCommand => i18n.t(MessageId::ApprovalCommandLabel),
        ApprovalRequestKind::TurnExtension => i18n.t(MessageId::ApprovalTurnExtensionLabel),
    };
    let next_label = next_pending.map(|next| format!("{} {}", next.id, next.subject));
    let action_set = approval_action_set_for(request, &state.approvals.requests);
    let selected_action = state
        .approvals
        .focus
        .get(&request.id)
        .copied()
        // A focused action can fall out of the card's current set when the
        // pending queue shrinks (TurnConsent -> Standard); fall back to
        // Approve so the highlight matches what input will submit.
        .filter(|action| action_set.action_index(*action).is_some())
        .unwrap_or(ApprovalPanelAction::Approve);
    let expanded = state.approvals.expanded_cards.contains(&request.id);
    let turn_consent = matches!(
        action_set,
        ApprovalActionSet::TurnConsent | ApprovalActionSet::TurnConsentHighRisk
    );
    let turn_extension = action_set == ApprovalActionSet::TurnExtension;
    let deny_always_trust = matches!(
        action_set,
        ApprovalActionSet::StandardHighRisk | ApprovalActionSet::TurnConsentHighRisk
    );
    let irrecoverable = request
        .assessment
        .as_ref()
        .is_some_and(assessment_is_irrecoverable);
    // Unknown tools are the one medium-risk exception to the fail-quiet ARP:
    // Trust mode deliberately stops at this boundary, so the card must tell
    // the user why an otherwise trusted turn is waiting.
    let card_reason = (state.approval_mode == CoshApprovalMode::Trust
        && request.kind == ApprovalRequestKind::Tool
        && known_provider_tool(&request.subject).is_none())
    .then(|| {
        state
            .i18n()
            .t(MessageId::ApprovalTrustUnknownToolReason)
            .to_string()
    })
    .or_else(|| {
        request.assessment.as_ref().and_then(|assessment| {
            crate::ui::card_reason_phrase(request.risk, assessment.primary_reason, state.i18n())
        })
    });
    let height = RatatuiInlineRenderer::for_terminal()
        .with_language(state.language)
        .write_approval_panel(
            output,
            ApprovalPanelModel {
                id: &request.id,
                kind: request.kind.label(),
                risk: request.risk,
                reason: card_reason.as_deref(),
                subject: &request.subject,
                preview_label,
                preview: &request.preview,
                queue_position: pending_position,
                queue_total: pending_total,
                next_label: next_label.as_deref(),
                selected_action,
                expanded,
                turn_consent,
                turn_extension,
                deny_always_trust,
                irrecoverable,
                hook_warnings: request
                    .hook_warnings
                    .iter()
                    .map(|w| HookWarningView {
                        hook_name: w.hook_name.as_str(),
                        message: w.message.as_str(),
                        decision: w.decision.as_deref(),
                    })
                    .collect(),
            },
        )?;
    state.approvals.active_panel_id = Some(request.id.clone());
    state.approvals.active_panel_height = height;
    Ok(())
}

pub(crate) fn redraw_current_approval_request<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_approval_panel(state, output)?;
    render_current_approval_request(state, output)
}

pub(crate) fn clear_active_approval_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let height = state.approvals.active_panel_height;
    if height == 0 {
        state.approvals.active_panel_id = None;
        return Ok(());
    }

    write!(output, "\x1b[{height}A")?;
    for row in 0..height {
        write!(output, "\r\x1b[2K")?;
        if row + 1 < height {
            write!(output, "\x1b[1B")?;
        }
    }
    if height > 1 {
        write!(output, "\x1b[{}A", height - 1)?;
    }
    write!(output, "\r")?;
    state.approvals.active_panel_id = None;
    state.approvals.active_panel_height = 0;
    Ok(())
}

pub(crate) fn approval_is_pending(state: &InlineState, id: &str) -> bool {
    state
        .approvals
        .requests
        .iter()
        .any(|request| request.id == id && request.status == ApprovalRequestStatus::Pending)
}

pub(crate) fn approval_focus_from_event(
    event: &ShellEvent,
    requests: &[RuntimeApprovalRequest],
) -> Option<(String, ApprovalPanelAction)> {
    if event.kind != ShellEventKind::UserInputIntercepted
        || event.component.as_deref() != Some("card")
        || event.message.as_deref() != Some("focus")
    {
        return None;
    }

    let (id, selected) = event.input.as_deref()?.split_once(':')?;
    let index = selected.trim().parse::<usize>().ok()?;
    let action_set = requests
        .iter()
        .find(|request| request.id == id.trim())
        .map(|request| approval_action_set_for(request, requests))
        .unwrap_or(ApprovalActionSet::Standard);
    let action = action_set.action_at(index)?;
    Some((id.trim().to_string(), action))
}
