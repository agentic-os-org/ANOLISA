//! Which pending approvals may be presented by the standalone UI worker, and the
//! read-only projection the worker is allowed to see.
//!
//! Two boundaries matter here and both are deliberate:
//!
//! - Eligibility is a pure predicate. It reads state and returns either a frozen
//!   identity plus a projection, or a named reason. It never spawns, writes, or
//!   mutates, so a rejection is always a complete return to the inline path
//!   rather than a half-migrated card.
//! - The projection carries presentation facts only. Nothing that could decide,
//!   dispatch, or audit an approval crosses to the worker: no tool input, no
//!   provider ids, no callbacks. The worker paints what it is given and the Host
//!   remains the only authority on what the approval means.

use crate::approval::approved_tool::request_is_executable_bash_tool;
use crate::approval::broker::{classify_approval_outcome, ApprovalOutcome, ApprovalOutcomeInput};
use crate::approval::panel::{approval_action_set_for, assessment_is_irrecoverable};
use crate::approval::resolution::request_can_receive_host_executed_result;
use crate::approval::runtime::active_run_owns_provider_approval;
use crate::runtime::approval_ui::ApprovalSliceActivation;
use crate::runtime::controller::pending_card_capture;
use crate::runtime::prelude::*;
use crate::ui::worker_protocol::{ApprovalActionView, ApprovalCardView, UiLanguage};

/// Business identity of one approval action, frozen when the request enters the
/// slice.
///
/// These are the fields the existing approval audit and provider result already
/// correlate on, so the slice adds no new public identifier. The key exists so a
/// later dispatch reservation can be keyed on something that survives a worker
/// rebuild, which a presentation revision does not.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ApprovalActionKey {
    pub(crate) shell_session_id: String,
    pub(crate) run_id: String,
    pub(crate) approval_id: String,
    pub(crate) request_id: String,
    pub(crate) tool_use_id: String,
}

/// Why a pending approval stays on the inline path.
///
/// One variant per eligibility rule, so deleting any single guard turns exactly
/// one case in the reject matrix red instead of silently widening the slice.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SliceRejectReason {
    /// No worker policy was admitted.
    NotActivated,
    /// The request is no longer pending.
    NotPending,
    /// The request is not an executable Bash tool request.
    NotExecutableBashTool,
    /// The request did not arrive as a provider control permission.
    NotControlPermission,
    /// The provider request or tool-use id is missing.
    MissingProviderIds,
    /// Hook approvals keep their own presentation.
    HookRequest,
    /// Turn-extension approvals offer a different action set.
    TurnExtensionRequest,
    /// No active provider run owns this request.
    NotOwnedByActiveRun,
    /// The owning provider cannot receive a host-executed shell result, so the
    /// Host could not complete the handoff it would be promising.
    ProviderCannotReceiveHostResult,
    /// An approved outcome would not classify as a foreground shell handoff.
    NotForegroundShellHandoff,
    /// More than one approval is pending, so which card the worker owns is
    /// ambiguous.
    NotSolePendingApproval,
    /// The resolved action set is not the three-action high-risk set.
    NotStandardHighRiskActionSet,
    /// Another card owner has higher priority for input capture.
    HigherPriorityCardOwner,
}

/// A pending approval cleared for worker presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EligibleApprovalSlice {
    /// Frozen business identity.
    pub(crate) action_key: ApprovalActionKey,
    /// Read-only projection to send to the worker.
    pub(crate) card: ApprovalCardView,
}

/// Decides whether `request` may be presented by the standalone worker.
///
/// Every rule is a complete gate: the slice is taken only when all of them hold,
/// and any rejection means the caller presents the request inline exactly as it
/// does today.
///
/// # Errors
/// Returns the first [`SliceRejectReason`] whose rule fails.
pub(crate) fn approval_slice_eligibility(
    activation: Option<&ApprovalSliceActivation>,
    state: &InlineState,
    request: &RuntimeApprovalRequest,
    revision: u64,
) -> Result<EligibleApprovalSlice, SliceRejectReason> {
    // Activation is all-or-nothing upstream, so its presence is the whole rule:
    // an `ApprovalSliceActivation` cannot exist in a partially configured state.
    if activation.is_none() {
        return Err(SliceRejectReason::NotActivated);
    }
    if request.status != ApprovalRequestStatus::Pending {
        return Err(SliceRejectReason::NotPending);
    }
    // Hook and turn-extension requests are ruled out before the tool-shape rules.
    // Ordering matters: a `HOOK:` subject is never a shell tool name and a
    // TurnExtension is never `ApprovalRequestKind::Tool`, so checking the tool
    // shape first would absorb both into `NotExecutableBashTool` and leave these
    // two guards untestable and silently deletable.
    if request.hook_requires_approval || request.subject.starts_with("HOOK:") {
        return Err(SliceRejectReason::HookRequest);
    }
    if request.kind == ApprovalRequestKind::TurnExtension {
        return Err(SliceRejectReason::TurnExtensionRequest);
    }
    if !request_is_executable_bash_tool(request) {
        return Err(SliceRejectReason::NotExecutableBashTool);
    }
    if !request.provider_shell_request_kind.is_control_permission() {
        return Err(SliceRejectReason::NotControlPermission);
    }
    let (Some(request_id), Some(tool_use_id)) = (&request.request_id, &request.tool_use_id) else {
        return Err(SliceRejectReason::MissingProviderIds);
    };
    let owned = state
        .agent_run
        .active
        .as_ref()
        .is_some_and(|run| active_run_owns_provider_approval(run, request));
    if !owned {
        return Err(SliceRejectReason::NotOwnedByActiveRun);
    }
    if !request_can_receive_host_executed_result(state, request) {
        return Err(SliceRejectReason::ProviderCannotReceiveHostResult);
    }
    // Classified with a hypothetical Approved status on purpose: the request is
    // still Pending here, and `approval_outcome_for_request` reads the current
    // status, so asking it would classify every eligible request as a rejection.
    let hypothetical = classify_approval_outcome(ApprovalOutcomeInput {
        approved: true,
        shell_tool: request_is_executable_bash_tool(request),
        provider_request: request.provider_shell_request_kind.is_control_permission(),
    });
    if hypothetical != ApprovalOutcome::ForegroundShellHandoff {
        return Err(SliceRejectReason::NotForegroundShellHandoff);
    }
    let pending = state
        .approvals
        .requests
        .iter()
        .filter(|other| other.status == ApprovalRequestStatus::Pending)
        .count();
    if pending != 1 {
        return Err(SliceRejectReason::NotSolePendingApproval);
    }
    if approval_action_set_for(request, &state.approvals.requests)
        != ApprovalActionSet::StandardHighRisk
    {
        return Err(SliceRejectReason::NotStandardHighRiskActionSet);
    }
    // Reusing the capture ladder rather than restating it: whichever card the
    // input router would hand keystrokes to must be this approval, which covers
    // every higher-priority owner in one place and cannot drift from it.
    let owns_input = matches!(
        pending_card_capture(state),
        Some(RawInputCapture::Approval { ref id, .. }) if id == &request.id
    );
    if !owns_input {
        return Err(SliceRejectReason::HigherPriorityCardOwner);
    }

    let selected = focused_action(state, request);
    let expanded = state.approvals.expanded_cards.contains(&request.id);
    let card = approval_card_view(revision, state, request, selected, expanded)
        .ok_or(SliceRejectReason::NotStandardHighRiskActionSet)?;
    Ok(EligibleApprovalSlice {
        action_key: ApprovalActionKey {
            shell_session_id: request.session_id.clone(),
            run_id: request.run_id.clone(),
            approval_id: request.id.clone(),
            request_id: request_id.clone(),
            tool_use_id: tool_use_id.clone(),
        },
        card,
    })
}

/// The action the card currently highlights, falling back to Approve.
///
/// A focus entry can outlive the action set that produced it when the pending
/// queue shrinks, so an unrecognised action is treated as no focus rather than as
/// a reason to leave the slice.
pub(crate) fn focused_action(
    state: &InlineState,
    request: &RuntimeApprovalRequest,
) -> ApprovalPanelAction {
    let action_set = approval_action_set_for(request, &state.approvals.requests);
    state
        .approvals
        .focus
        .get(&request.id)
        .copied()
        .filter(|action| action_set.action_index(*action).is_some())
        .unwrap_or(ApprovalPanelAction::Approve)
}

/// Builds the read-only card projection for one presentation of `request`.
///
/// Returns `None` when the request's action set is not the three-action set the
/// slice can present, or when the focused action is not one the worker knows. The
/// caller then presents inline; nothing is truncated to make the slice fit.
pub(crate) fn approval_card_view(
    revision: u64,
    state: &InlineState,
    request: &RuntimeApprovalRequest,
    selected: ApprovalPanelAction,
    expanded: bool,
) -> Option<ApprovalCardView> {
    let action_set = approval_action_set_for(request, &state.approvals.requests);
    if action_set != ApprovalActionSet::StandardHighRisk {
        return None;
    }
    let actions = action_set
        .descriptors()
        .iter()
        .map(|descriptor| action_view(descriptor.action))
        .collect::<Option<Vec<_>>>()?;
    let selected = action_view(selected)?;
    let i18n = state.i18n();
    let reason = request
        .assessment
        .as_ref()
        .and_then(|assessment| {
            crate::ui::card_reason_phrase(request.risk, assessment.primary_reason, i18n)
        })
        .unwrap_or_default()
        .to_string();
    // The preview is already redacted by the same path the inline card uses; the
    // worker never receives tool input or provider identifiers.
    Some(ApprovalCardView {
        revision,
        language: ui_language(state.language),
        display_id: request.id.clone(),
        kind: request.kind.label().to_string(),
        risk: request.risk.to_string(),
        reason,
        subject: request.subject.clone(),
        preview: request.preview.clone(),
        // Eligibility forces `ApprovalRequestKind::Tool`, so this is the same
        // label the inline card shows above the command preview.
        label: i18n.t(MessageId::ApprovalToolInputLabel).to_string(),
        actions,
        selected,
        expanded,
        irrecoverable: request
            .assessment
            .as_ref()
            .is_some_and(assessment_is_irrecoverable),
    })
}

fn action_view(action: ApprovalPanelAction) -> Option<ApprovalActionView> {
    match action {
        ApprovalPanelAction::Approve => Some(ApprovalActionView::Approve),
        ApprovalPanelAction::Deny => Some(ApprovalActionView::Deny),
        ApprovalPanelAction::Details => Some(ApprovalActionView::Details),
        // AlwaysTrust and ApproveTurn are not offered on a high-risk card, and
        // inventing a worker-side equivalent would widen what the slice can
        // promise on the user's behalf.
        ApprovalPanelAction::AlwaysTrust | ApprovalPanelAction::ApproveTurn => None,
    }
}

fn ui_language(language: Language) -> UiLanguage {
    match language {
        Language::EnUs => UiLanguage::En,
        Language::ZhCn => UiLanguage::Zh,
    }
}
