use std::io::{self, Write};

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Widget},
};

use super::approval::{
    hook_warning_icon, metadata_subject, model_action_set, risk_level_label, wrapped_preview_rows,
    ApprovalPanelModel,
};
use super::approval_actions::approval_action_styled_rows;
use super::approval_reason::{approval_reason_rows, approval_reason_styled_lines};
use super::approval_warning::approval_warning_rows;
use super::{
    buffer_to_lines, buffer_to_styled_lines, line_to_string, wrap_plain_line, RatatuiInlineRenderer,
};
use crate::MessageId;

impl RatatuiInlineRenderer {
    pub(crate) fn write_approval_details_view<W: Write>(
        &self,
        output: &mut W,
        model: &ApprovalPanelModel<'_>,
        page: usize,
    ) -> io::Result<(usize, usize)> {
        let (lines, selected_page) = self.approval_details_page_lines(model, page);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok((lines.len(), selected_page))
    }

    pub(super) fn approval_details_view_lines(
        &self,
        model: &ApprovalPanelModel<'_>,
        page: usize,
    ) -> Vec<String> {
        self.approval_details_page_lines(model, page).0
    }

    fn approval_details_page_lines(
        &self,
        model: &ApprovalPanelModel<'_>,
        page: usize,
    ) -> (Vec<String>, usize) {
        let i18n = self.i18n();
        let width = self.panel_standard_width();
        let content_width = if self.plain {
            width.max(1) as usize
        } else {
            width.saturating_sub(4).max(1) as usize
        };
        let risk = risk_level_label(model.risk, i18n);
        let subject = metadata_subject(model.subject, content_width, &risk, "");
        let mut header = wrap_plain_line(&format!("{subject} · {risk}"), content_width);
        if let Some(reason) = model.reason.filter(|_| !model.irrecoverable) {
            header.extend(approval_reason_rows(reason, content_width, i18n));
        }
        let warnings = approval_warning_rows(model, content_width, i18n);
        let command = model.output_command.unwrap_or(model.preview);
        let mut body = wrapped_preview_rows(command, content_width, usize::MAX);
        if model.kind == "shell command request"
            || crate::tools::is_shell_tool_name(model.subject.trim_start_matches("tool "))
        {
            body.extend(wrap_plain_line(
                i18n.t(MessageId::ApprovalTerminalBoundaryDetails),
                content_width,
            ));
            body.extend(wrap_plain_line(
                i18n.t(MessageId::ApprovalPreviewBoundaryDetails),
                content_width,
            ));
        }
        if model.turn_consent {
            body.extend(wrap_plain_line(
                i18n.t(MessageId::ApprovalTurnScopeDetails),
                content_width,
            ));
        }
        for warning in &model.hook_warnings {
            body.extend(wrap_plain_line(
                &format!(
                    "{} {}: {}",
                    hook_warning_icon(warning.decision),
                    warning.hook_name,
                    warning.message
                ),
                content_width,
            ));
        }
        let actions = approval_action_styled_rows(
            model_action_set(model),
            model.selected_action,
            i18n,
            content_width,
        );
        let limit = self
            .row_limit
            .unwrap_or(if width <= 40 { 16 } else { 24 })
            .saturating_sub(1) as usize;
        let hint_rows =
            wrap_plain_line(i18n.t(MessageId::ApprovalDetailsPagingKeys), content_width);
        let title = format!("{} {}", i18n.t(MessageId::ApprovalDetailsTitle), model.id);
        let title_rows = wrap_plain_line(&title, content_width);
        let frame_rows = if self.plain { title_rows.len() } else { 2 };
        let fixed =
            frame_rows + 1 + header.len() + warnings.len() + actions.len() + hint_rows.len();
        if model.irrecoverable {
            body.extend(wrap_plain_line(
                i18n.t(MessageId::ApprovalIrrecoverableWarningLine),
                content_width,
            ));
        }
        let page_size = limit.saturating_sub(fixed).max(1);
        let pages = body.len().max(1).div_ceil(page_size);
        let selected_page = page.min(pages.saturating_sub(1));
        let start = selected_page * page_size;
        let mut lines = header.into_iter().map(Line::from).collect::<Vec<_>>();
        let mut output_explanation = false;
        lines.extend(warnings.into_iter().map(|(text, color)| {
            if text.starts_with('└') || text.starts_with("  ") {
                output_explanation = true;
                approval_reason_styled_lines(vec![text], color, i18n).remove(0)
            } else if output_explanation && color != Color::Reset {
                Line::from(Span::styled(text, Style::default().fg(Color::DarkGray)))
            } else {
                output_explanation = false;
                Line::from(Span::styled(text, Style::default().fg(color)))
            }
        }));
        lines.extend(
            body.iter()
                .skip(start)
                .take(page_size)
                .cloned()
                .map(Line::from),
        );
        let position = i18n.format(
            MessageId::ApprovalDetailsPagePosition,
            &[
                ("page", &(selected_page + 1).to_string()),
                ("pages", &pages.to_string()),
            ],
        );
        lines.push(Line::from(Span::styled(
            position,
            Style::default().fg(Color::DarkGray),
        )));
        lines.extend(hint_rows.into_iter().map(Line::from));
        lines.extend(actions);
        if self.plain {
            let mut rendered = title_rows;
            rendered.extend(
                lines
                    .iter()
                    .flat_map(|line| wrap_plain_line(&line_to_string(line), content_width)),
            );
            return (rendered, selected_page);
        }
        let height = (lines.len() + 2) as u16;
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        let block = Block::bordered()
            .border_style(Style::default().fg(if model.risk == "high" {
                Color::Red
            } else {
                Color::Yellow
            }))
            .title(format!(" {title} "));
        let inner = block.inner(area);
        block.render(area, &mut buffer);
        let content = Rect::new(inner.x + 1, inner.y, content_width as u16, inner.height);
        Paragraph::new(Text::from(lines)).render(content, &mut buffer);
        let rendered = if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        };
        (rendered, selected_page)
    }
}
