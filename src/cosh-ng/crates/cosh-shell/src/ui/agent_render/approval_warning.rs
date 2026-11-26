use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget};

use super::approval::{hook_warning_color, hook_warning_icon, ApprovalPanelModel};
use super::approval_reason::{approval_reason_line, approval_reason_styled_lines};
use super::wrap_plain_line;
use crate::tools::command_risk_output::{shell_output_facts, ShellOutputFacts};
use crate::{I18n, MessageId};

pub(super) fn approval_warning_rows(
    model: &ApprovalPanelModel<'_>,
    width: usize,
    i18n: I18n,
) -> Vec<(String, Color)> {
    let mut rows = Vec::new();
    if model.irrecoverable {
        let full = format!(
            "\u{26a0} {}",
            i18n.t(MessageId::ApprovalIrrecoverableWarningLine)
        );
        let warning = if width > 40 && wrap_plain_line(&full, width).len() <= 2 {
            full
        } else {
            format!(
                "\u{26a0} {}",
                i18n.t(MessageId::ApprovalIrrecoverableWarningCompact)
            )
        };
        rows.extend(
            wrap_plain_line(&warning, width)
                .into_iter()
                .map(|row| (row, Color::Red)),
        );
    }
    let shell = model.kind == "shell command request"
        || (model.kind == "tool request" && crate::tools::is_shell_tool_name(model.subject));
    let mut messages = Vec::new();
    let risk_color = if model.risk == "high" {
        Color::Red
    } else {
        Color::Yellow
    };
    if shell {
        let facts = model
            .output_command
            .map(shell_output_facts)
            .unwrap_or(ShellOutputFacts {
                unknown: true,
                ..Default::default()
            });
        for (present, message) in [
            (facts.environment, MessageId::ApprovalOutputEnvironment),
            (facts.sensitive, MessageId::ApprovalOutputSensitive),
            (facts.arguments, MessageId::ApprovalOutputArguments),
        ] {
            if present {
                messages.push((message, risk_color));
            }
        }
        if messages.len() > 1 {
            let sources = [
                (
                    facts.environment,
                    MessageId::ApprovalOutputSourceEnvironment,
                ),
                (facts.sensitive, MessageId::ApprovalOutputSourceSensitive),
                (facts.arguments, MessageId::ApprovalOutputSourceArguments),
            ]
            .into_iter()
            .filter(|(present, _)| *present)
            .map(|(_, message)| i18n.t(message))
            .collect::<Vec<_>>()
            .join(" / ");
            let warning = if width <= 40 && facts.environment && facts.sensitive && facts.arguments
            {
                i18n.t(MessageId::ApprovalOutputCombinedCompact).to_string()
            } else {
                i18n.format(
                    MessageId::ApprovalOutputCombined,
                    &[("sources", sources.as_str())],
                )
            };
            rows.extend(
                output_explanation_rows(&warning, model.reason.is_some(), width, i18n)
                    .into_iter()
                    .map(|row| (row, risk_color)),
            );
            messages.clear();
        }
        if facts.unknown && !facts.environment && !facts.sensitive && !facts.arguments {
            messages.push((MessageId::ApprovalOutputUnknown, Color::Reset));
        }
    }
    if model.turn_consent
        && model.selected_action == super::actions::ApprovalPanelAction::ApproveTurn
    {
        messages.push((MessageId::ApprovalTurnOutputUnredacted, Color::Reset));
    }
    for (message, color) in messages {
        let explanation = matches!(
            message,
            MessageId::ApprovalOutputEnvironment
                | MessageId::ApprovalOutputSensitive
                | MessageId::ApprovalOutputArguments
        );
        let message_rows = if explanation {
            output_explanation_rows(i18n.t(message), model.reason.is_some(), width, i18n)
        } else {
            wrap_plain_line(i18n.t(message), width)
        };
        rows.extend(message_rows.into_iter().map(|row| (row, color)));
    }
    rows
}

fn output_explanation_rows(
    text: &str,
    continuation: bool,
    width: usize,
    i18n: I18n,
) -> Vec<String> {
    if continuation {
        wrap_plain_line(text, width.saturating_sub(2))
            .into_iter()
            .map(|row| format!("  {row}"))
            .collect()
    } else {
        wrap_plain_line(&approval_reason_line(text, i18n), width)
    }
}

pub(super) fn approval_preview_limit(
    model: &ApprovalPanelModel<'_>,
    width: u16,
    i18n: I18n,
    height_limit: Option<usize>,
) -> usize {
    let maximum = if model.expanded { 6 } else { 3 };
    let Some(height_limit) = height_limit.or_else(|| (width <= 40).then_some(15)) else {
        return maximum;
    };
    let content_width = super::approval::approval_content_width(width);
    let fixed_rows = approval_fixed_rows(model, content_width, i18n);
    let queue_rows = usize::from(approval_queue_details_visible(
        model,
        width,
        i18n,
        Some(height_limit),
    )) * if super::approval::is_command_approval_request(model) {
        usize::from(model.queue_total > 1)
    } else {
        usize::from(model.next_label.is_some())
    };
    height_limit
        .saturating_sub(fixed_rows + queue_rows)
        .clamp(1, maximum)
}

pub(super) fn approval_queue_details_visible(
    model: &ApprovalPanelModel<'_>,
    width: u16,
    i18n: I18n,
    height_limit: Option<usize>,
) -> bool {
    if width <= 40 {
        return false;
    }
    height_limit.is_none_or(|limit| {
        approval_fixed_rows(model, super::approval::approval_content_width(width), i18n) + 2
            <= limit
    })
}

fn approval_fixed_rows(model: &ApprovalPanelModel<'_>, content_width: usize, i18n: I18n) -> usize {
    let actions = super::approval::model_action_set(model);
    let action_rows =
        super::approval_actions::approval_action_row_count(actions, i18n, content_width);
    let reason_rows = model
        .reason
        .map(|reason| {
            super::approval_reason::approval_reason_rows(reason, content_width, i18n).len()
        })
        .unwrap_or(0);
    let hook_rows = approval_hook_warning_lines(model, content_width, i18n).len();
    3 + action_rows
        + reason_rows
        + hook_rows
        + approval_warning_rows(model, content_width, i18n).len()
}

pub(super) fn approval_hook_warning_lines(
    model: &ApprovalPanelModel<'_>,
    width: usize,
    i18n: I18n,
) -> Vec<Line<'static>> {
    let lines = model
        .hook_warnings
        .iter()
        .flat_map(|warning| {
            let summary = format!(
                "│ {} {}: {}",
                hook_warning_icon(warning.decision),
                warning.hook_name,
                warning.message
            );
            wrap_plain_line(&summary, width).into_iter().map(|row| {
                Line::from(Span::styled(
                    row,
                    Style::default().fg(hook_warning_color(warning.decision)),
                ))
            })
        })
        .collect::<Vec<_>>();
    if lines.len() <= 2 && !model.irrecoverable {
        return lines;
    }
    let warning = model
        .hook_warnings
        .iter()
        .max_by_key(|warning| match warning.decision {
            Some("block" | "deny") => 2,
            Some("ask") => 1,
            _ => 0,
        });
    let Some(warning) = warning else {
        return lines;
    };
    if model.irrecoverable
        || (model.reason.is_some()
            && model.turn_consent
            && model.selected_action == super::actions::ApprovalPanelAction::ApproveTurn
            && !approval_warning_rows(model, width, i18n).is_empty())
    {
        let count = model.hook_warnings.len().to_string();
        let suffix = i18n.format(
            MessageId::ApprovalHookNoticeCompact,
            &[("hook", ""), ("count", &count)],
        );
        let budget = width.saturating_sub(super::display_width(&suffix) + 2);
        let mut hook = warning.hook_name.to_string();
        if super::display_width(&hook) > budget {
            while super::display_width(&hook) + 1 > budget {
                if hook.pop().is_none() {
                    break;
                }
            }
            hook.push('…');
        }
        return vec![Line::from(Span::styled(
            format!(
                "{} {}",
                hook_warning_icon(warning.decision),
                i18n.format(
                    MessageId::ApprovalHookNoticeCompact,
                    &[("hook", hook.as_str()), ("count", &count)],
                )
            ),
            Style::default().fg(hook_warning_color(warning.decision)),
        ))];
    }
    let first = super::approval::wrapped_preview_rows(
        &format!(
            "{} {}: {}",
            hook_warning_icon(warning.decision),
            warning.hook_name,
            warning.message
        ),
        width,
        1,
    )
    .remove(0);
    let summary = i18n.format(
        MessageId::ApprovalHookNoticeSummary,
        &[("count", &model.hook_warnings.len().to_string())],
    );
    vec![
        Line::from(Span::styled(
            first,
            Style::default().fg(hook_warning_color(warning.decision)),
        )),
        Line::from(Span::styled(summary, Style::default().fg(Color::DarkGray))),
    ]
}

pub(super) fn render_approval_warnings(
    rows: Vec<(String, Color)>,
    area: Rect,
    buffer: &mut Buffer,
    i18n: I18n,
) {
    let mut output_explanation = false;
    let lines = rows
        .into_iter()
        .map(|(row, color)| {
            if row.starts_with('└') || row.starts_with("  ") {
                output_explanation = true;
                approval_reason_styled_lines(vec![row], color, i18n).remove(0)
            } else if output_explanation && color != Color::Reset {
                Line::from(Span::styled(row, Style::default().fg(Color::DarkGray)))
            } else {
                output_explanation = false;
                Line::from(Span::styled(
                    row,
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ))
            }
        })
        .collect::<Vec<_>>();
    Paragraph::new(Text::from(lines)).render(area, buffer);
}
