use std::io::{self, Write};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{block::Padding, Block, Paragraph, Widget},
};

use super::{
    buffer_to_lines, buffer_to_styled_lines, wrap_plain_line, RatatuiInlineRenderer, MAX_WIDTH,
};

#[derive(Debug, Clone)]
pub struct ApprovalReceiptPanelModel<'a> {
    pub title: &'a str,
    pub negative: bool,
    pub id: &'a str,
    pub kind: &'a str,
    pub decision: &'a str,
    pub subject: &'a str,
    pub preview: &'a str,
    pub message: &'a str,
}

impl RatatuiInlineRenderer {
    pub fn write_approval_receipt_panel<W: Write>(
        &self,
        output: &mut W,
        model: ApprovalReceiptPanelModel<'_>,
    ) -> io::Result<usize> {
        let lines = self.approval_receipt_panel_write_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub fn approval_receipt_panel_lines(
        &self,
        model: ApprovalReceiptPanelModel<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_approval_receipt_panel_lines(model);
        }
        if !approval_receipt_has_body(&model) {
            return compact_approval_receipt_lines(&model, self.styles_enabled());
        }

        let i18n = self.i18n();
        let width = self.panel_standard_width();
        let height = approval_receipt_panel_height(&i18n, &model, width);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_approval_receipt_panel(&i18n, model, area, &mut buffer);
        buffer_to_lines(&buffer, area)
    }

    fn approval_receipt_panel_write_lines(
        &self,
        model: ApprovalReceiptPanelModel<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_approval_receipt_panel_lines(model);
        }
        if !approval_receipt_has_body(&model) {
            return compact_approval_receipt_lines(&model, self.styles_enabled());
        }

        let i18n = self.i18n();
        let width = self.panel_standard_width();
        let height = approval_receipt_panel_height(&i18n, &model, width);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_approval_receipt_panel(&i18n, model, area, &mut buffer);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }

    fn plain_approval_receipt_panel_lines(
        &self,
        model: ApprovalReceiptPanelModel<'_>,
    ) -> Vec<String> {
        let i18n = self.i18n();
        if !approval_receipt_has_body(&model) {
            return vec![format!("{} {}", model.title, model.id)];
        }

        let width = self.panel_standard_width();
        let content_width = approval_receipt_content_width(width);
        let mut lines = vec![format!("{} {}", model.title, model.id)];
        if !model.preview.is_empty() {
            lines.extend(wrapped_receipt_rows(
                receipt_preview_label(&i18n, model.subject),
                model.preview,
                content_width,
            ));
        }
        if !model.message.is_empty() {
            lines.extend(wrap_plain_line(model.message, content_width));
        }
        lines
    }
}

fn approval_receipt_panel_height(
    i18n: &crate::I18n,
    model: &ApprovalReceiptPanelModel<'_>,
    width: u16,
) -> u16 {
    if !approval_receipt_has_body(model) {
        return 1;
    }

    let content_width = approval_receipt_content_width(width);
    let preview_rows = if model.preview.is_empty() {
        0
    } else {
        wrapped_receipt_rows(
            receipt_preview_label(i18n, model.subject),
            model.preview,
            content_width,
        )
        .len() as u16
    };
    let message_rows = if model.message.is_empty() {
        0
    } else {
        wrap_plain_line(model.message, content_width).len().max(1) as u16
    };
    2 + preview_rows + message_rows
}

fn compact_approval_receipt_lines(
    model: &ApprovalReceiptPanelModel<'_>,
    styled: bool,
) -> Vec<String> {
    if !styled {
        return vec![compact_approval_receipt_text(model)];
    }

    let area = Rect::new(0, 0, MAX_WIDTH, 1);
    let mut buffer = Buffer::empty(area);
    Paragraph::new(Line::from(Span::styled(
        compact_approval_receipt_text(model),
        receipt_status_style(model),
    )))
    .render(area, &mut buffer);
    buffer_to_styled_lines(&buffer, area)
}

fn compact_approval_receipt_text(model: &ApprovalReceiptPanelModel<'_>) -> String {
    if !model.subject.is_empty() {
        format!("{} {} \u{2014} {}", model.title, model.id, model.subject)
    } else {
        format!("{} {}", model.title, model.id)
    }
}

fn receipt_status_style(model: &ApprovalReceiptPanelModel<'_>) -> Style {
    let color = if model.negative {
        Color::Red
    } else {
        Color::Green
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn render_approval_receipt_panel(
    i18n: &crate::I18n,
    model: ApprovalReceiptPanelModel<'_>,
    area: Rect,
    buffer: &mut Buffer,
) {
    let border = if model.negative {
        Color::Red
    } else {
        Color::Green
    };
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Line::from(vec![
            Span::styled(
                format!(" {} ", model.title),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("{} ", model.id)),
        ]))
        .border_style(Style::default().fg(border));
    let inner = block.inner(area);
    block.render(area, buffer);

    if !approval_receipt_has_body(&model) {
        return;
    }

    let content_width = inner.width as usize;
    let mut constraints = Vec::new();
    if !model.preview.is_empty() {
        constraints.push(Constraint::Length(
            wrapped_receipt_rows(
                receipt_preview_label(i18n, model.subject),
                model.preview,
                content_width,
            )
            .len() as u16,
        ));
    }
    if !model.message.is_empty() {
        constraints.push(Constraint::Length(
            wrap_plain_line(model.message, content_width).len().max(1) as u16,
        ));
    }
    let chunks = Layout::vertical(constraints).split(inner);

    let mut chunk_index = 0;
    if !model.preview.is_empty() {
        let preview_label = receipt_preview_label(i18n, model.subject);
        let preview_lines = wrapped_receipt_rows(preview_label, model.preview, content_width)
            .into_iter()
            .enumerate()
            .map(|(idx, line)| {
                if idx == 0 {
                    styled_receipt_label_line(line, preview_label)
                } else {
                    Line::from(Span::raw(line))
                }
            })
            .collect::<Vec<_>>();
        Paragraph::new(preview_lines).render(chunks[chunk_index], buffer);
        chunk_index += 1;
    }
    if !model.message.is_empty() {
        Paragraph::new(wrap_plain_line(model.message, content_width).join("\n"))
            .render(chunks[chunk_index], buffer);
    }
}

fn approval_receipt_has_body(model: &ApprovalReceiptPanelModel<'_>) -> bool {
    !model.preview.is_empty() || !model.message.is_empty()
}

pub(super) fn receipt_preview_label(i18n: &crate::I18n, subject: &str) -> &'static str {
    let subject = subject.to_ascii_lowercase();
    if subject.contains("bash") || subject.contains("shell") {
        i18n.t(crate::MessageId::ApprovalCommandLabel)
    } else {
        i18n.t(crate::MessageId::ApprovalJournalPreviewLabel)
    }
}

fn approval_receipt_content_width(width: u16) -> usize {
    width.saturating_sub(4).max(20) as usize
}

fn wrapped_receipt_rows(label: &str, text: &str, width: usize) -> Vec<String> {
    let prefix = format!("{label}: ");
    let continuation = " ".repeat(prefix.chars().count());
    let content_width = width.saturating_sub(prefix.chars().count()).max(1);
    wrap_plain_line(text, content_width)
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 0 {
                format!("{prefix}{line}")
            } else {
                format!("{continuation}{line}")
            }
        })
        .collect()
}

fn styled_receipt_label_line(line: String, label: &str) -> Line<'static> {
    let prefix = format!("{label}: ");
    let prefix_len = prefix.len().min(line.len());
    let (prefix, rest) = line.split_at(prefix_len);
    Line::from(vec![
        Span::styled(
            prefix.to_string(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw(rest.to_string()),
    ])
}
