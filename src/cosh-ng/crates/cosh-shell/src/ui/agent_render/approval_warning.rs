use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Paragraph, Widget};

use super::approval::{wrapped_preview_rows, ApprovalPanelModel};
use crate::{I18n, MessageId};

pub(super) fn approval_warning_rows(
    model: &ApprovalPanelModel<'_>,
    width: usize,
    i18n: I18n,
) -> Vec<(String, Color)> {
    let mut rows = Vec::new();
    if model.irrecoverable {
        rows.push((
            format!(
                "\u{26a0} {}",
                i18n.t(MessageId::ApprovalIrrecoverableWarningLine)
            ),
            Color::Red,
        ));
    }
    let disclosure = if model.turn_consent {
        MessageId::ApprovalTurnOutputUnredacted
    } else if model.kind == "shell command request"
        || (model.kind == "tool request" && crate::tools::is_shell_tool_name(model.subject))
    {
        MessageId::ApprovalCommandOutputUnredacted
    } else {
        return rows;
    };
    for text in [
        i18n.t(disclosure),
        i18n.t(MessageId::ApprovalOutputScrollback),
    ] {
        rows.extend(
            wrapped_preview_rows(text, width, usize::MAX)
                .into_iter()
                .map(|row| (row, Color::Yellow)),
        );
    }
    rows
}

pub(super) fn render_approval_warnings(
    rows: Vec<(String, Color)>,
    area: Rect,
    buffer: &mut Buffer,
) {
    let lines = rows
        .into_iter()
        .map(|(row, color)| {
            Line::from(Span::styled(
                row,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ))
        })
        .collect::<Vec<_>>();
    Paragraph::new(Text::from(lines)).render(area, buffer);
}
