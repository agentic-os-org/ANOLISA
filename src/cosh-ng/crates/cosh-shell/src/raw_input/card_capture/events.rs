use crate::question::choices::question_choice_count as shared_question_choice_count;
use crate::ui::{ApprovalActionSet, ApprovalPanelAction};

use super::{RawInputCapture, RawInputEvent};

pub(super) fn cancel_event(capture: &RawInputCapture) -> RawInputEvent {
    match capture {
        RawInputCapture::Approval { id, .. } | RawInputCapture::Consultation { id } => {
            RawInputEvent::CardCancel(id.clone())
        }
        RawInputCapture::Mode { id, .. } => RawInputEvent::ModeCancel(id.clone()),
        RawInputCapture::Config { id, .. } => RawInputEvent::ConfigCancel(id.clone()),
        RawInputCapture::ConfigLanguage { id, .. } => {
            RawInputEvent::ConfigLanguageCancel(id.clone())
        }
        RawInputCapture::Session { id, .. } => RawInputEvent::SessionCancel(id.clone()),
        RawInputCapture::Question { id, .. } | RawInputCapture::TextQuestion { id, .. } => {
            RawInputEvent::QuestionCancel(id.clone())
        }
        RawInputCapture::Evidence { id } => RawInputEvent::EvidenceCancel(id.clone()),
        RawInputCapture::PromptDraft { id, .. } => {
            RawInputEvent::PromptDraftCancel { id: id.clone() }
        }
    }
}

pub(in crate::raw_input) fn releases_capture(event: &RawInputEvent) -> bool {
    matches!(
        event,
        RawInputEvent::CardApprove(_)
            | RawInputEvent::CardApproveTurn(_)
            | RawInputEvent::CardAlwaysTrust(_)
            | RawInputEvent::CardDeny(_)
            | RawInputEvent::CardCancel(_)
            | RawInputEvent::CardAnswer(_)
            | RawInputEvent::CardSecretAnswer(_)
            | RawInputEvent::ModeSet(_, _)
            | RawInputEvent::ModeCancel(_)
            | RawInputEvent::ConfigSave(_)
            | RawInputEvent::ConfigCancel(_)
            | RawInputEvent::ConfigLanguageSet(_, _)
            | RawInputEvent::ConfigLanguageCancel(_)
            | RawInputEvent::SessionResume(_, _)
            | RawInputEvent::SessionClearConfirm(_)
            | RawInputEvent::SessionCancel(_)
            | RawInputEvent::QuestionCancel(_)
            | RawInputEvent::QuestionAbort(_)
            | RawInputEvent::EvidenceSend(_)
            | RawInputEvent::EvidenceIgnore(_)
            | RawInputEvent::EvidenceCancel(_)
            | RawInputEvent::PromptDraftSubmit { .. }
            | RawInputEvent::PromptDraftCancel { .. }
    )
}

pub(super) fn card_answer_event(answer: &str, secret: bool) -> RawInputEvent {
    if secret {
        RawInputEvent::CardSecretAnswer(answer.to_string())
    } else {
        RawInputEvent::CardAnswer(answer.to_string())
    }
}

pub(super) fn empty_question_submission(id: &str, secret: bool) -> RawInputEvent {
    if secret {
        RawInputEvent::CardSecretAnswer(String::new())
    } else {
        RawInputEvent::QuestionSubmitAttempt(id.to_string())
    }
}

pub(super) fn is_csi_final_byte(byte: u8) -> bool {
    (0x40..=0x7e).contains(&byte)
}

/// Action set carried by an approval capture; non-approval card captures
/// fall back to the standard list.
pub(super) fn capture_action_set(capture: &RawInputCapture) -> ApprovalActionSet {
    match capture {
        RawInputCapture::Approval { action_set, .. } => *action_set,
        _ => ApprovalActionSet::Standard,
    }
}

pub(super) fn selected_options_answer(selected_options: &[usize]) -> String {
    selected_options
        .iter()
        .map(|index| (index + 1).to_string())
        .collect::<Vec<_>>()
        .join(",")
}

pub(super) fn is_removed_question_answer_slash(answer: &str) -> bool {
    answer.split_whitespace().next() == Some("/answer")
}

pub(super) fn is_removed_question_answer_slash_fragment(answer: &str) -> bool {
    let answer = answer.trim_start();
    !answer.is_empty()
        && ("/answer".starts_with(answer) || answer.split_whitespace().next() == Some("/answer"))
}

pub(super) fn approval_event_for_action(id: &str, action: ApprovalPanelAction) -> RawInputEvent {
    match action {
        ApprovalPanelAction::Approve => RawInputEvent::CardApprove(id.to_string()),
        ApprovalPanelAction::ApproveTurn => RawInputEvent::CardApproveTurn(id.to_string()),
        ApprovalPanelAction::AlwaysTrust => RawInputEvent::CardAlwaysTrust(id.to_string()),
        ApprovalPanelAction::Deny => RawInputEvent::CardDeny(id.to_string()),
        ApprovalPanelAction::Details => RawInputEvent::CardDetails(id.to_string()),
    }
}

pub(super) fn question_choice_count(capture: &RawInputCapture) -> usize {
    match capture {
        RawInputCapture::Question {
            option_count,
            allow_free_text,
            ..
        } => shared_question_choice_count(*option_count, *allow_free_text),
        RawInputCapture::TextQuestion { .. } => 0,
        RawInputCapture::Approval { .. }
        | RawInputCapture::Consultation { .. }
        | RawInputCapture::Evidence { .. }
        | RawInputCapture::Session { .. }
        | RawInputCapture::Mode { .. }
        | RawInputCapture::Config { .. }
        | RawInputCapture::ConfigLanguage { .. }
        | RawInputCapture::PromptDraft { .. } => 0,
    }
}

pub(super) fn capture_initial_selection(capture: &RawInputCapture) -> usize {
    match capture {
        RawInputCapture::Question { selected, .. } => {
            (*selected).min(question_choice_count(capture).saturating_sub(1))
        }
        RawInputCapture::Mode {
            selected,
            option_count,
            ..
        }
        | RawInputCapture::Config {
            selected,
            option_count,
            ..
        }
        | RawInputCapture::ConfigLanguage {
            selected,
            option_count,
            ..
        }
        | RawInputCapture::Session {
            selected,
            option_count,
            ..
        } => (*selected).min(option_count.saturating_sub(1)),
        _ => 0,
    }
}
