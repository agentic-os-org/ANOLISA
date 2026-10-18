use std::io::{self, Write};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border::ROUNDED,
    text::{Line, Span, Text},
    widgets::{block::Padding, Block, Paragraph, Widget, Wrap},
};

use super::actions::{ApprovalActionSet, ApprovalPanelAction};
use super::approval_actions::{
    action_span, approval_action_label, approval_action_plain_rows, approval_action_row_count,
    approval_action_styled_rows, hook_approval_action_line, hook_approval_action_spans,
    packed_approval_actions,
};
use super::approval_reason::{
    approval_reason_line, approval_reason_rows, approval_reason_styled_lines,
};
use super::approval_warning::{irrecoverable_warning_text, render_irrecoverable_warning};

use super::{
    buffer_to_lines, buffer_to_styled_lines, char_width, display_width, RatatuiInlineRenderer,
};
use crate::types::CardKind;

#[derive(Debug, Clone)]
pub struct ApprovalPanelModel<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub risk: &'a str,
    pub reason: Option<&'a str>,
    pub subject: &'a str,
    pub preview_label: &'a str,
    pub preview: &'a str,
    pub queue_position: usize,
    pub queue_total: usize,
    pub next_label: Option<&'a str>,
    pub selected_action: ApprovalPanelAction,
    pub expanded: bool,
    /// Offer the turn-scope batch consent action (issue #1773): true when
    /// the run already has ≥ 2 approval requests (queued or resolved).
    pub turn_consent: bool,
    /// Offer only Continue and Stop for a persisted capped Agent run.
    pub turn_extension: bool,
    /// Withhold the AlwaysTrust action for high-risk requests (#2064).
    pub deny_always_trust: bool,
    /// Render the irrecoverable-consequence warning line (#2064): set when
    /// the assessment carries a system-control side effect.
    pub irrecoverable: bool,
    pub hook_warnings: Vec<HookWarningView<'a>>,
}

/// View model for a single hook warning in the approval panel.
#[derive(Debug, Clone)]
pub struct HookWarningView<'a> {
    pub hook_name: &'a str,
    pub message: &'a str,
    pub decision: Option<&'a str>,
}

impl RatatuiInlineRenderer {
    pub fn write_approval_panel<W: Write>(
        &self,
        output: &mut W,
        model: ApprovalPanelModel<'_>,
    ) -> io::Result<usize> {
        let lines = self.approval_panel_write_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub fn approval_panel_lines(&self, model: ApprovalPanelModel<'_>) -> Vec<String> {
        if self.plain {
            return self.plain_approval_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let height = approval_panel_height(&model, width, self.i18n());
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_approval_panel(model, self.i18n(), area, &mut buffer);
        buffer_to_lines(&buffer, area)
    }

    fn approval_panel_write_lines(&self, model: ApprovalPanelModel<'_>) -> Vec<String> {
        if self.plain {
            return self.plain_approval_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let height = approval_panel_height(&model, width, self.i18n());
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_approval_panel(model, self.i18n(), area, &mut buffer);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }

    fn plain_approval_panel_lines(&self, model: ApprovalPanelModel<'_>) -> Vec<String> {
        let i18n = self.i18n();
        if is_command_approval_request(&model) {
            let command_rows = command_preview_rows(
                model.preview,
                self.content_width(),
                max_preview_rows(model.expanded),
            );
            let mut lines = vec![
                CardKind::Permission.title(i18n.t(crate::MessageId::ApprovalRequiredTitle)),
                command_request_heading(model.subject, i18n).to_string(),
            ];
            for warning in &model.hook_warnings {
                let icon = hook_warning_icon(warning.decision);
                lines.push(format!("\u{2502} {icon} {}", warning.hook_name));
                for msg_line in warning.message.lines() {
                    lines.push(format!("\u{2502}   {msg_line}"));
                }
            }
            if let Some(reason) = model.reason {
                lines.push(approval_reason_line(reason, i18n));
            }
            if model.irrecoverable {
                lines.push(irrecoverable_warning_text(i18n));
            }
            lines.extend(command_rows);
            if model.queue_total > 1 {
                let position = model.queue_position.to_string();
                let total = model.queue_total.to_string();
                let mut queue = i18n.format(
                    crate::MessageId::ApprovalQueueCompactLine,
                    &[("position", position.as_str()), ("total", total.as_str())],
                );
                if let Some(next) = model.next_label {
                    queue.push_str(
                        &i18n.format(crate::MessageId::ApprovalQueueNextSuffix, &[("next", next)]),
                    );
                }
                lines.push(queue);
            }
            lines.extend(approval_action_plain_rows(
                model_action_set(&model),
                model.selected_action,
                i18n,
                self.content_width(),
            ));
            if model.expanded {
                lines.push(
                    i18n.t(crate::MessageId::ApprovalCommandDefaultPolicy)
                        .to_string(),
                );
                lines.push(format!(
                    "{}{}",
                    i18n.t(crate::MessageId::ApprovalKeysPrefix),
                    i18n.t(crate::MessageId::ApprovalKeysText)
                ));
            }
            return lines;
        }

        if is_hook_approval_request(&model) {
            let mut lines =
                vec![CardKind::Permission.title(i18n.t(crate::MessageId::ApprovalHookHeading))];
            for warning in &model.hook_warnings {
                let icon = hook_warning_icon(warning.decision);
                lines.push(format!("\u{2502} {icon} {}", warning.hook_name));
                for msg_line in warning.message.lines() {
                    lines.push(format!("\u{2502}   {msg_line}"));
                }
            }
            if model.queue_total > 1 {
                let position = model.queue_position.to_string();
                let total = model.queue_total.to_string();
                lines.push(i18n.format(
                    crate::MessageId::ApprovalQueueCompactLine,
                    &[("position", position.as_str()), ("total", total.as_str())],
                ));
            }
            lines.push(hook_approval_action_line(model.selected_action, i18n));
            return lines;
        }

        // V6a slim generic card (ARP-R8): title, single metadata row
        // `{subject} · {risk badge}[ · queue N/M]`, optional High-risk
        // continuation line, preview, optional next, actions; key hints and
        // policy only when expanded.
        let risk_label = risk_level_label(model.risk, i18n);
        let queue_suffix = queue_meta_suffix(&model, i18n);
        let subject = metadata_subject(
            model.subject,
            self.content_width(),
            &risk_label,
            &queue_suffix,
        );
        let mut lines = vec![
            CardKind::Permission.title(i18n.t(crate::MessageId::ApprovalRequiredTitle)),
            format!("{subject} · {risk_label}{queue_suffix}"),
        ];
        if let Some(reason) = model.reason {
            lines.push(approval_reason_line(reason, i18n));
        }
        if model.irrecoverable {
            lines.push(irrecoverable_warning_text(i18n));
        }
        for warning in &model.hook_warnings {
            let icon = hook_warning_icon(warning.decision);
            lines.push(format!("\u{2502} {icon} {}", warning.hook_name));
            for msg_line in warning.message.lines() {
                lines.push(format!("\u{2502}   {msg_line}"));
            }
        }
        lines.push(model.preview.to_string());
        if let Some(next) = model.next_label {
            lines.push(format!(
                "{}{next}",
                i18n.t(crate::MessageId::ApprovalNextLabel)
            ));
        }
        lines.extend(approval_action_plain_rows(
            model_action_set(&model),
            model.selected_action,
            i18n,
            self.content_width(),
        ));
        if model.expanded {
            lines.push(
                i18n.t(crate::MessageId::ApprovalExecutableToolPolicy)
                    .to_string(),
            );
            lines.push(format!(
                "{}{}",
                i18n.t(crate::MessageId::ApprovalKeysPrefix),
                i18n.t(crate::MessageId::ApprovalKeysText)
            ));
        }
        lines
    }
}

fn approval_panel_height(model: &ApprovalPanelModel<'_>, width: u16, i18n: crate::I18n) -> u16 {
    let content_width = approval_content_width(width);
    let action_rows =
        approval_action_row_count(model_action_set(model), i18n, content_width) as u16;
    let hook_warning_rows = model
        .hook_warnings
        .iter()
        .map(|w| 1 + w.message.lines().count()) // 1 for hookName line + N message lines
        .sum::<usize>() as u16;
    if is_hook_approval_request(model) {
        // heading + hook_warnings + queue(opt) + actions + keys + border(2)
        let queue_rows = u16::from(model.queue_total > 1);
        return 3 + action_rows + hook_warning_rows + queue_rows;
    }
    if is_command_approval_request(model) {
        let command_rows = command_preview_rows(
            model.preview,
            content_width,
            max_preview_rows(model.expanded),
        )
        .len()
        .max(1) as u16;
        let queue_rows = u16::from(model.queue_total > 1);
        let reason_rows = model
            .reason
            .map(|reason| {
                approval_reason_rows(
                    reason,
                    content_width,
                    crate::I18n::new(crate::Language::EnUs),
                )
                .len() as u16
            })
            .unwrap_or(0);
        let expanded_rows = if model.expanded { 2 } else { 0 };
        let warning_rows = u16::from(model.irrecoverable);
        return 3
            + action_rows
            + command_rows
            + queue_rows
            + reason_rows
            + warning_rows
            + expanded_rows
            + hook_warning_rows;
    }

    let preview_rows = wrapped_preview_rows(
        model.preview,
        content_width,
        max_preview_rows(model.expanded),
    )
    .len()
    .max(1) as u16;
    let next_rows = u16::from(model.next_label.is_some());
    let reason_rows = model
        .reason
        .map(|reason| {
            approval_reason_rows(
                reason,
                content_width,
                crate::I18n::new(crate::Language::EnUs),
            )
            .len() as u16
        })
        .unwrap_or(0);
    // V6a slim generic card: border(2) + metadata(1) + actions(N) + content;
    // keys(1) + policy(2) only when expanded (ARP-R8).
    let expanded_rows = if model.expanded { 3 } else { 0 };
    let warning_rows = u16::from(model.irrecoverable);
    3 + action_rows
        + preview_rows
        + next_rows
        + reason_rows
        + warning_rows
        + expanded_rows
        + hook_warning_rows
}

fn render_approval_panel(
    model: ApprovalPanelModel<'_>,
    i18n: crate::I18n,
    area: Rect,
    buffer: &mut Buffer,
) {
    if is_command_approval_request(&model) {
        render_command_tool_approval_panel(model, i18n, area, buffer);
        return;
    }
    if is_hook_approval_request(&model) {
        render_hook_approval_panel(model, i18n, area, buffer);
        return;
    }

    let border = if model.risk == "high" {
        Color::Red
    } else {
        Color::Yellow
    };
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(
                format!(
                    " {} ",
                    CardKind::Permission.title(i18n.t(crate::MessageId::ApprovalTitle))
                ),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{} ", model.id)),
        ]))
        .border_set(ROUNDED)
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    block.render(area, buffer);

    let preview_rows = wrapped_preview_rows(
        model.preview,
        inner.width.saturating_sub(2) as usize,
        max_preview_rows(model.expanded),
    );
    let preview_height = preview_rows.len().max(1) as u16;
    let next_height = u16::from(model.next_label.is_some());
    let reason_rows = model
        .reason
        .map(|reason| approval_reason_rows(reason, inner.width.saturating_sub(2) as usize, i18n))
        .unwrap_or_default();
    let reason_height = reason_rows.len() as u16;
    let hook_warning_height = model
        .hook_warnings
        .iter()
        .map(|w| 1 + w.message.lines().count())
        .sum::<usize>() as u16;
    // V6a slim generic card: metadata row, optional continuation line,
    // irrecoverable warning (#2064), hook warnings, preview, optional
    // next, actions; keys + policy only when expanded (ARP-R8).
    let action_lines = approval_action_styled_rows(
        model_action_set(&model),
        model.selected_action,
        i18n,
        inner.width as usize,
    );
    let warning_height = u16::from(model.irrecoverable);
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Length(reason_height),
        Constraint::Length(warning_height),
        Constraint::Length(hook_warning_height),
        Constraint::Length(preview_height),
        Constraint::Length(next_height),
        Constraint::Length(action_lines.len() as u16),
    ];
    if model.expanded {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(2));
    }
    let chunks = Layout::vertical(constraints).split(inner);

    // Metadata row: `{subject} · {risk badge}[ · queue N/M]` — the badge is
    // localized (ARP-R7); High keeps the border color, low/medium are dimmed.
    // The subject is ellipsized so the risk badge and queue info always keep
    // their reserved width (review follow-up: long MCP tool names must never
    // push the risk signal out of view).
    let risk_style = if model.risk == "high" {
        Style::default().fg(border)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let risk_label = risk_level_label(model.risk, i18n);
    let queue_suffix = queue_meta_suffix(&model, i18n);
    let subject = metadata_subject(
        model.subject,
        inner.width.saturating_sub(2) as usize,
        &risk_label,
        &queue_suffix,
    );
    Paragraph::new(Line::from(vec![
        Span::styled(subject, Style::default().fg(Color::Cyan)),
        Span::styled(" · ", Style::default().fg(Color::DarkGray)),
        Span::styled(risk_label, risk_style),
        Span::styled(queue_suffix, Style::default().fg(Color::DarkGray)),
    ]))
    .render(chunks[0], buffer);

    if !reason_rows.is_empty() {
        Paragraph::new(Text::from(approval_reason_styled_lines(
            reason_rows,
            border,
            i18n,
        )))
        .render(chunks[1], buffer);
    }

    if model.irrecoverable {
        render_irrecoverable_warning(i18n, chunks[2], buffer);
    }

    if !model.hook_warnings.is_empty() {
        let mut warning_lines: Vec<Line<'_>> = Vec::new();
        for w in &model.hook_warnings {
            let color = hook_warning_color(w.decision);
            let icon = hook_warning_icon(w.decision);
            // Line 1: colored left bar + icon + bold hookName
            warning_lines.push(Line::from(vec![
                Span::styled(format!("\u{2502} {icon} "), Style::default().fg(color)),
                Span::styled(
                    w.hook_name.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ]));
            // Message lines: colored left bar + indented text, split on newlines
            for msg_line in w.message.lines() {
                warning_lines.push(Line::from(vec![
                    Span::styled("\u{2502}   ", Style::default().fg(color)),
                    Span::raw(msg_line.to_string()),
                ]));
            }
        }
        Paragraph::new(Text::from(warning_lines)).render(chunks[3], buffer);
    }

    let preview_lines = preview_rows
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Style::default().fg(Color::White))))
        .collect::<Vec<_>>();
    Paragraph::new(Text::from(preview_lines)).render(chunks[4], buffer);

    if let Some(next) = model.next_label {
        Paragraph::new(Line::from(vec![
            Span::styled(
                i18n.t(crate::MessageId::ApprovalNextLabel),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(next.to_string()),
        ]))
        .render(chunks[5], buffer);
    }

    Paragraph::new(Text::from(action_lines)).render(chunks[6], buffer);

    if model.expanded {
        Paragraph::new(Line::from(vec![
            Span::styled(
                i18n.t(crate::MessageId::ApprovalKeysPrefix),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(i18n.t(crate::MessageId::ApprovalKeysText)),
        ]))
        .render(chunks[7], buffer);
        Paragraph::new(Text::from(vec![
            Line::from(i18n.t(crate::MessageId::ApprovalExecutableToolPolicy)),
            Line::from(i18n.t(crate::MessageId::ApprovalExecutableToolPolicyExtra)),
        ]))
        .wrap(Wrap { trim: true })
        .render(chunks[8], buffer);
    }
}

fn render_command_tool_approval_panel(
    model: ApprovalPanelModel<'_>,
    i18n: crate::I18n,
    area: Rect,
    buffer: &mut Buffer,
) {
    let border = if model.risk == "high" {
        Color::Red
    } else {
        Color::Yellow
    };
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(
                format!(
                    " {} ",
                    CardKind::Permission.title(i18n.t(crate::MessageId::ApprovalTitle))
                ),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{} ", model.id)),
        ]))
        .border_set(ROUNDED)
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    block.render(area, buffer);
    let command_rows = command_preview_rows(
        model.preview,
        inner.width.saturating_sub(2) as usize,
        max_preview_rows(model.expanded),
    );
    let reason_rows = model
        .reason
        .map(|reason| approval_reason_rows(reason, inner.width.saturating_sub(2) as usize, i18n))
        .unwrap_or_default();
    let hook_warning_height = model
        .hook_warnings
        .iter()
        .map(|w| 1 + w.message.lines().count())
        .sum::<usize>() as u16;
    let queue_height = u16::from(model.queue_total > 1);
    let warning_height = u16::from(model.irrecoverable);
    let action_lines = approval_action_styled_rows(
        model_action_set(&model),
        model.selected_action,
        i18n,
        inner.width as usize,
    );
    let mut constraints = vec![
        Constraint::Length(1),
        Constraint::Length(hook_warning_height),
        Constraint::Length(reason_rows.len() as u16),
        Constraint::Length(warning_height),
        Constraint::Length(command_rows.len().max(1) as u16),
        Constraint::Length(queue_height),
        Constraint::Length(action_lines.len() as u16),
    ];
    if model.expanded {
        constraints.push(Constraint::Length(1));
        constraints.push(Constraint::Length(1));
    }
    let chunks = Layout::vertical(constraints).split(inner);
    let action_index = 6;
    let keys_index = 7;
    let policy_index = 8;

    Paragraph::new(Line::from(Span::styled(
        command_request_heading(model.subject, i18n),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )))
    .render(chunks[0], buffer);

    if !model.hook_warnings.is_empty() {
        let mut warning_lines: Vec<Line<'_>> = Vec::new();
        for w in &model.hook_warnings {
            let color = hook_warning_color(w.decision);
            let icon = hook_warning_icon(w.decision);
            warning_lines.push(Line::from(vec![
                Span::styled(format!("\u{2502} {icon} "), Style::default().fg(color)),
                Span::styled(
                    w.hook_name.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ]));
            for msg_line in w.message.lines() {
                warning_lines.push(Line::from(vec![
                    Span::styled("\u{2502}   ", Style::default().fg(color)),
                    Span::raw(msg_line.to_string()),
                ]));
            }
        }
        Paragraph::new(Text::from(warning_lines)).render(chunks[1], buffer);
    }

    if !reason_rows.is_empty() {
        Paragraph::new(Text::from(approval_reason_styled_lines(
            reason_rows,
            border,
            i18n,
        )))
        .render(chunks[2], buffer);
    }

    if model.irrecoverable {
        render_irrecoverable_warning(i18n, chunks[3], buffer);
    }

    let command_lines = command_rows
        .into_iter()
        .map(|line| Line::from(Span::styled(line, Style::default().fg(Color::White))))
        .collect::<Vec<_>>();
    Paragraph::new(Text::from(command_lines)).render(chunks[4], buffer);

    if model.queue_total > 1 {
        let position = model.queue_position.to_string();
        let total = model.queue_total.to_string();
        let mut queue = i18n.format(
            crate::MessageId::ApprovalQueueCompactLine,
            &[("position", position.as_str()), ("total", total.as_str())],
        );
        if let Some(next) = model.next_label {
            queue.push_str(
                &i18n.format(crate::MessageId::ApprovalQueueNextSuffix, &[("next", next)]),
            );
        }
        Paragraph::new(Line::from(Span::styled(
            queue,
            Style::default().fg(Color::DarkGray),
        )))
        .render(chunks[5], buffer);
    }

    Paragraph::new(Text::from(action_lines)).render(chunks[action_index], buffer);

    if model.expanded {
        Paragraph::new(Line::from(vec![
            Span::styled(
                i18n.t(crate::MessageId::ApprovalKeysPrefix),
                Style::default().fg(Color::DarkGray),
            ),
            Span::raw(i18n.t(crate::MessageId::ApprovalKeysText)),
        ]))
        .render(chunks[keys_index], buffer);
        Paragraph::new(i18n.t(crate::MessageId::ApprovalCommandDefaultPolicy))
            .wrap(Wrap { trim: true })
            .render(chunks[policy_index], buffer);
    }
}

// ─── Hook Approval Panel ─────────────────────────────────────────────

fn render_hook_approval_panel(
    model: ApprovalPanelModel<'_>,
    i18n: crate::I18n,
    area: Rect,
    buffer: &mut Buffer,
) {
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(
                format!(
                    " {} ",
                    CardKind::Permission.title(i18n.t(crate::MessageId::ApprovalHookHeading))
                ),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{} ", model.id)),
        ]))
        .border_set(ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    block.render(area, buffer);

    let hook_warning_height = model
        .hook_warnings
        .iter()
        .map(|w| 1 + w.message.lines().count())
        .sum::<usize>() as u16;
    let queue_height = u16::from(model.queue_total > 1);
    let constraints = vec![
        Constraint::Length(hook_warning_height),
        Constraint::Length(queue_height),
        Constraint::Length(1), // actions
        Constraint::Length(1), // keys
    ];
    let chunks = Layout::vertical(constraints).split(inner);

    // Hook warnings (main content)
    if !model.hook_warnings.is_empty() {
        let mut warning_lines: Vec<Line<'_>> = Vec::new();
        for w in &model.hook_warnings {
            let color = hook_warning_color(w.decision);
            let icon = hook_warning_icon(w.decision);
            warning_lines.push(Line::from(vec![
                Span::styled(format!("\u{2502} {icon} "), Style::default().fg(color)),
                Span::styled(
                    w.hook_name.to_string(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
            ]));
            for msg_line in w.message.lines() {
                warning_lines.push(Line::from(vec![
                    Span::styled("\u{2502}   ", Style::default().fg(color)),
                    Span::raw(msg_line.to_string()),
                ]));
            }
        }
        Paragraph::new(Text::from(warning_lines)).render(chunks[0], buffer);
    }

    // Queue position
    if model.queue_total > 1 {
        let position = model.queue_position.to_string();
        let total = model.queue_total.to_string();
        Paragraph::new(Line::from(Span::styled(
            i18n.format(
                crate::MessageId::ApprovalQueueCompactLine,
                &[("position", position.as_str()), ("total", total.as_str())],
            ),
            Style::default().fg(Color::DarkGray),
        )))
        .render(chunks[1], buffer);
    }

    // Actions: Allow once / Deny / Details (no Always trust)
    Paragraph::new(hook_approval_action_spans(model.selected_action, i18n))
        .render(chunks[2], buffer);

    // Keys
    Paragraph::new(Line::from(vec![
        Span::styled(
            i18n.t(crate::MessageId::ApprovalKeysPrefix),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(i18n.t(crate::MessageId::ApprovalKeysText)),
    ]))
    .render(chunks[3], buffer);
}

/// Action set offered by this card (single source of truth mirrors
/// `approval_action_set_for` on the request side, issue #1773; the
/// high-risk variants drop AlwaysTrust, issue #2064).
fn model_action_set(model: &ApprovalPanelModel<'_>) -> ApprovalActionSet {
    if is_hook_approval_request(model) {
        ApprovalActionSet::Hook
    } else if model.turn_extension {
        ApprovalActionSet::TurnExtension
    } else if model.turn_consent {
        if model.deny_always_trust {
            ApprovalActionSet::TurnConsentHighRisk
        } else {
            ApprovalActionSet::TurnConsent
        }
    } else if model.deny_always_trust {
        ApprovalActionSet::StandardHighRisk
    } else {
        ApprovalActionSet::Standard
    }
}

/// Localized risk badge value (ARP-R7): high/medium/low map to i18n labels;
/// values outside the closed `legacy_risk()` domain fall back to a neutral
/// localized label so the badge never mixes languages (review follow-up).
fn risk_level_label(risk: &str, i18n: crate::I18n) -> String {
    let id = match risk {
        "high" => crate::MessageId::ApprovalRiskLevelHigh,
        "medium" => crate::MessageId::ApprovalRiskLevelMedium,
        "low" => crate::MessageId::ApprovalRiskLevelLow,
        _ => crate::MessageId::ApprovalRiskLevelUnknown,
    };
    i18n.t(id).to_string()
}

/// Metadata-row queue suffix, only rendered when more than one card is pending.
fn queue_meta_suffix(model: &ApprovalPanelModel<'_>, i18n: crate::I18n) -> String {
    if model.queue_total <= 1 {
        return String::new();
    }
    i18n.format(
        crate::MessageId::ApprovalQueueMetaSuffix,
        &[
            ("position", model.queue_position.to_string().as_str()),
            ("total", model.queue_total.to_string().as_str()),
        ],
    )
}

/// Ellipsize the metadata-row subject so the risk badge and queue suffix
/// always fit: unbounded custom/MCP tool names must never push the risk
/// signal past the row end (review follow-up on #1786).
fn metadata_subject(
    subject: &str,
    content_width: usize,
    risk_label: &str,
    queue_suffix: &str,
) -> String {
    const SEPARATOR_WIDTH: usize = 3; // " · "
    const MIN_SUBJECT_WIDTH: usize = 8;
    let reserved = SEPARATOR_WIDTH + display_width(risk_label) + display_width(queue_suffix);
    let budget = content_width
        .saturating_sub(reserved)
        .max(MIN_SUBJECT_WIDTH);
    if display_width(subject) <= budget {
        return subject.to_string();
    }
    let mut truncated = String::new();
    let mut width = 0;
    for ch in subject.chars() {
        let ch_width = char_width(ch);
        if width + ch_width > budget.saturating_sub(1) {
            break;
        }
        truncated.push(ch);
        width += ch_width;
    }
    truncated.push('\u{2026}');
    truncated
}

pub(super) fn approval_content_width(width: u16) -> usize {
    width.saturating_sub(4).max(20) as usize
}

/// Renders the `audit_ref` row for the approval details and journal panels.
///
/// Both surfaces must derive their panel height from the same row count, so the
/// reference is wrapped here instead of at each call site. Event ids are longer
/// than the 40-column minimum panel, and a truncated id cannot be traced back to
/// the audit log — so the row wraps rather than clipping at the border.
///
/// Returns an empty vec when no reference exists, which keeps the panel height
/// unchanged and avoids a placeholder row.
pub(super) fn audit_ref_rows(audit_ref: Option<&str>, content_width: usize) -> Vec<String> {
    let Some(audit_ref) = audit_ref else {
        return Vec::new();
    };
    let text = audit_ref_line(audit_ref);
    // One row beyond the worst case keeps `wrapped_preview_rows` from
    // ellipsizing an id that must stay verbatim.
    let max_rows = display_width(&text).div_ceil(content_width.max(20)) + 1;
    wrapped_preview_rows(&text, content_width, max_rows)
}

/// Field name is a stable technical identifier shared with `/audit`; never localize it.
pub(super) fn audit_ref_line(audit_ref: &str) -> String {
    format!("audit_ref: {audit_ref}")
}

fn max_preview_rows(expanded: bool) -> usize {
    if expanded {
        6
    } else {
        3
    }
}

fn is_command_approval_request(model: &ApprovalPanelModel<'_>) -> bool {
    (model.kind == "tool request"
        && (model.subject.eq_ignore_ascii_case("tool Bash")
            || model.subject.eq_ignore_ascii_case("tool shell")))
        || (model.kind == "shell command request"
            && model.subject.eq_ignore_ascii_case("shell command"))
}

fn is_hook_approval_request(model: &ApprovalPanelModel<'_>) -> bool {
    model.subject.starts_with("HOOK:")
}

fn command_request_heading(subject: &str, i18n: crate::I18n) -> &'static str {
    if subject.eq_ignore_ascii_case("tool shell") || subject.eq_ignore_ascii_case("shell command") {
        i18n.t(crate::MessageId::ApprovalRunShellCommandPrompt)
    } else {
        i18n.t(crate::MessageId::ApprovalRunBashCommandPrompt)
    }
}

fn command_preview_rows(command: &str, width: usize, max_rows: usize) -> Vec<String> {
    let rows = wrapped_preview_rows(command, width.saturating_sub(2).max(20), max_rows);
    if rows.is_empty() {
        return vec!["$".to_string()];
    }
    rows.into_iter()
        .enumerate()
        .map(|(idx, row)| {
            if idx == 0 {
                format!("$ {row}")
            } else {
                format!("  {row}")
            }
        })
        .collect()
}

pub(super) fn wrapped_preview_rows(text: &str, width: usize, max_rows: usize) -> Vec<String> {
    let width = width.max(20);
    let mut rows = Vec::new();
    for raw_line in text.lines() {
        let mut current = String::new();
        let mut current_width = 0;
        for ch in raw_line.chars() {
            let ch_width = char_width(ch);
            if current_width + ch_width > width && !current.is_empty() {
                rows.push(current);
                if rows.len() == max_rows {
                    return ellipsize_last_row(rows, width);
                }
                current = String::new();
                current_width = 0;
            }
            current.push(ch);
            current_width += ch_width;
        }
        if !current.is_empty() || raw_line.is_empty() {
            rows.push(current);
            if rows.len() == max_rows {
                return ellipsize_last_row(rows, width);
            }
        }
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn ellipsize_last_row(mut rows: Vec<String>, width: usize) -> Vec<String> {
    if let Some(last) = rows.last_mut() {
        while display_width(last) + 4 > width {
            if last.pop().is_none() {
                break;
            }
        }
        last.push_str(" ...");
    }
    rows
}

// ─── Hook warning decision-based styling ────────────────────────────

/// Color for hook warnings in ratatui rendering, selected by decision.
fn hook_warning_color(decision: Option<&str>) -> Color {
    match decision {
        Some("allow") | Some("approve") => Color::Green,
        Some("ask") => Color::Yellow,
        Some("block") | Some("deny") => Color::Red,
        _ => Color::Yellow,
    }
}

/// Decision icon for hook warnings.
pub(crate) fn hook_warning_icon(decision: Option<&str>) -> &'static str {
    match decision {
        Some("allow") | Some("approve") => "\u{2713}", // ✓
        Some("ask") => "?",
        Some("block") | Some("deny") => "\u{2717}", // ✗
        _ => "\u{2022}",                            // •
    }
}
