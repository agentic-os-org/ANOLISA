//! Approval-card action row rendering: hook single-row actions plus the
//! packed multi-row actions introduced with turn-scope batch consent
//! (issue #1773). Split out of `approval.rs` per the large-file policy;
//! everything here is pure "action set -> rendered rows" with no
//! dependency on `ApprovalPanelModel`.

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::actions::{pack_action_rows, ApprovalActionSet, ApprovalPanelAction};
use super::display_width;

/// Render action spans excluding "Always trust" for hook approval panels.
pub(super) fn hook_approval_action_spans(
    selected: ApprovalPanelAction,
    i18n: crate::I18n,
) -> Line<'static> {
    let mut spans = Vec::new();
    let mut first = true;
    for descriptor in ApprovalActionSet::Hook.descriptors() {
        if !first {
            spans.push(Span::raw("  "));
        }
        first = false;
        spans.push(action_span(
            approval_action_label(descriptor.action, ApprovalActionSet::Hook, i18n),
            descriptor.action,
            selected == descriptor.action,
        ));
    }
    Line::from(spans)
}

/// Plain-text action line excluding "Always trust" for hook approval panels.
pub(super) fn hook_approval_action_line(
    selected: ApprovalPanelAction,
    i18n: crate::I18n,
) -> String {
    ApprovalActionSet::Hook
        .descriptors()
        .iter()
        .map(|descriptor| {
            let label = approval_action_label(descriptor.action, ApprovalActionSet::Hook, i18n);
            if descriptor.action == selected {
                format!("[{label}]")
            } else {
                label.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("  ")
}

/// Pack the set's actions into display rows by label width; callers
/// pre-wrap so `Paragraph::wrap` never touches action rows.
pub(super) fn packed_approval_actions(
    set: ApprovalActionSet,
    i18n: crate::I18n,
    content_width: usize,
) -> Vec<Vec<ApprovalPanelAction>> {
    let descriptors = set.descriptors();
    let widths = descriptors
        .iter()
        .map(|descriptor| display_width(approval_action_label(descriptor.action, set, i18n)))
        .collect::<Vec<_>>();
    pack_action_rows(&widths, content_width)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|index| descriptors[index].action)
                .collect()
        })
        .collect()
}

pub(super) fn approval_action_row_count(
    set: ApprovalActionSet,
    i18n: crate::I18n,
    content_width: usize,
) -> usize {
    packed_approval_actions(set, i18n, content_width)
        .len()
        .max(1)
}

pub(super) fn approval_action_styled_rows(
    set: ApprovalActionSet,
    selected: ApprovalPanelAction,
    i18n: crate::I18n,
    content_width: usize,
) -> Vec<Line<'static>> {
    packed_approval_actions(set, i18n, content_width)
        .into_iter()
        .map(|row| {
            let mut spans = Vec::new();
            for (position, action) in row.into_iter().enumerate() {
                if position > 0 {
                    spans.push(Span::raw("  "));
                }
                spans.push(action_span(
                    approval_action_label(action, set, i18n),
                    action,
                    selected == action,
                ));
            }
            Line::from(spans)
        })
        .collect()
}

pub(super) fn approval_action_plain_rows(
    set: ApprovalActionSet,
    selected: ApprovalPanelAction,
    i18n: crate::I18n,
    content_width: usize,
) -> Vec<String> {
    packed_approval_actions(set, i18n, content_width)
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|action| {
                    let label = approval_action_label(action, set, i18n);
                    if action == selected {
                        format!("[{label}]")
                    } else {
                        label.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("  ")
        })
        .collect()
}

pub(super) fn approval_action_label(
    action: ApprovalPanelAction,
    set: ApprovalActionSet,
    i18n: crate::I18n,
) -> &'static str {
    if set == ApprovalActionSet::TurnExtension {
        match action {
            ApprovalPanelAction::Approve => {
                return i18n.t(crate::MessageId::ApprovalActionContinue);
            }
            ApprovalPanelAction::Deny => return i18n.t(crate::MessageId::ApprovalActionStop),
            _ => {}
        }
    }
    match action {
        ApprovalPanelAction::Approve => i18n.t(crate::MessageId::ApprovalActionAllowOnce),
        ApprovalPanelAction::ApproveTurn => i18n.t(crate::MessageId::ApprovalActionApproveTurn),
        ApprovalPanelAction::AlwaysTrust => i18n.t(crate::MessageId::ApprovalActionAlwaysTrust),
        ApprovalPanelAction::Deny => i18n.t(crate::MessageId::ApprovalActionDeny),
        ApprovalPanelAction::Details => i18n.t(crate::MessageId::ApprovalActionDetails),
    }
}

pub(super) fn action_span(
    label: &str,
    action: ApprovalPanelAction,
    selected: bool,
) -> Span<'static> {
    if selected {
        Span::styled(format!("> [ {label} ] "), selected_action_style(action))
    } else {
        Span::styled(format!("  [ {label} ] "), Style::default().fg(Color::Gray))
    }
}

fn selected_action_style(action: ApprovalPanelAction) -> Style {
    let background = match action {
        ApprovalPanelAction::Approve => Color::Green,
        ApprovalPanelAction::ApproveTurn => Color::Green,
        ApprovalPanelAction::AlwaysTrust => Color::Cyan,
        ApprovalPanelAction::Deny => Color::Red,
        ApprovalPanelAction::Details => Color::Blue,
    };
    Style::default()
        .fg(Color::White)
        .bg(background)
        .add_modifier(Modifier::BOLD)
}
