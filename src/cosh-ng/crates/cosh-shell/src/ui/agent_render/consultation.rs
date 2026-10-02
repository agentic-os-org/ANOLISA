use std::io::{self, Write};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border::ROUNDED,
    text::{Line, Span},
    widgets::{Block, Paragraph, Widget, Wrap},
};

use super::{buffer_to_lines, buffer_to_styled_lines, wrap_plain_line, RatatuiInlineRenderer};

#[derive(Debug, Clone)]
pub struct ConsultationCardModel {
    pub details_id: String,
    pub severity: String,
    pub title: String,
    pub finding: String,
    pub suggestion: String,
}

impl RatatuiInlineRenderer {
    pub fn write_consultation_card<W: Write>(
        &self,
        output: &mut W,
        model: &ConsultationCardModel,
    ) -> io::Result<usize> {
        let lines = self.consultation_card_write_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub fn consultation_card_lines(&self, model: &ConsultationCardModel) -> Vec<String> {
        if self.plain {
            return self.plain_consultation_card_lines(model);
        }

        let i18n = self.i18n();
        let width = self.panel_standard_width();
        let height = consultation_card_height(&i18n, model, width);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_consultation_card(&i18n, model, area, &mut buffer);
        buffer_to_lines(&buffer, area)
    }

    fn consultation_card_write_lines(&self, model: &ConsultationCardModel) -> Vec<String> {
        if self.plain {
            return self.plain_consultation_card_lines(model);
        }

        let i18n = self.i18n();
        let width = self.panel_standard_width();
        let height = consultation_card_height(&i18n, model, width);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_consultation_card(&i18n, model, area, &mut buffer);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }

    fn plain_consultation_card_lines(&self, model: &ConsultationCardModel) -> Vec<String> {
        let i18n = self.i18n();
        let content_width = self.content_width();
        let mut lines = vec![format!("{} [{}]:", model.title, model.severity)];
        lines.extend(wrap_plain_line(
            &format!("  {}", consultation_finding_line(&i18n, model)),
            content_width,
        ));
        lines.extend(wrap_plain_line(
            &format!("  {}", consultation_suggestion_line(&i18n, model)),
            content_width,
        ));
        lines.push(format!(
            "  [{}] [{}]",
            i18n.t(crate::MessageId::HookConsultationAnalyzeAction),
            i18n.t(crate::MessageId::HookConsultationIgnoreAction)
        ));
        lines
    }
}

fn consultation_card_height(i18n: &crate::I18n, model: &ConsultationCardModel, width: u16) -> u16 {
    let content_width = panel_content_width(width);
    let title_rows = wrapped_row_count(&model.title, content_width);
    let finding_rows = wrapped_row_count(&consultation_finding_line(i18n, model), content_width);
    let suggestion_rows =
        wrapped_row_count(&consultation_suggestion_line(i18n, model), content_width);
    let actions_rows = 1;
    title_rows + finding_rows + suggestion_rows + actions_rows + 2
}

fn render_consultation_card(
    i18n: &crate::I18n,
    model: &ConsultationCardModel,
    area: Rect,
    buffer: &mut Buffer,
) {
    let title_text = format!(" {} ", model.title);
    let severity_text = format!(" {} ", model.severity);

    let block = Block::bordered()
        .title(Line::from(vec![
            Span::styled(title_text, Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(
                format!("─── {} ", severity_text.trim()),
                Style::default()
                    .fg(severity_color(&model.severity))
                    .add_modifier(Modifier::BOLD),
            ),
        ]))
        .border_set(ROUNDED)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    block.render(area, buffer);

    let content_width = inner.width as usize;
    let title_rows = wrapped_row_count(&model.title, content_width);
    let finding_text = consultation_finding_line(i18n, model);
    let finding_rows = wrapped_row_count(&finding_text, content_width);
    let suggestion_text = consultation_suggestion_line(i18n, model);
    let suggestion_rows = wrapped_row_count(&suggestion_text, content_width);
    let actions_rows = 1u16;
    let chunks = Layout::vertical(vec![
        Constraint::Length(title_rows),
        Constraint::Length(finding_rows),
        Constraint::Length(suggestion_rows),
        Constraint::Length(actions_rows),
    ])
    .split(inner);

    Paragraph::new(model.title.as_str())
        .wrap(Wrap { trim: true })
        .render(chunks[0], buffer);

    Paragraph::new(finding_text)
        .wrap(Wrap { trim: true })
        .render(chunks[1], buffer);

    Paragraph::new(suggestion_text)
        .style(Style::default().fg(Color::Gray))
        .wrap(Wrap { trim: true })
        .render(chunks[2], buffer);

    Paragraph::new(Line::from(vec![
        Span::styled(
            format!(
                "[{}]",
                i18n.t(crate::MessageId::HookConsultationAnalyzeAction)
            ),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            format!(
                "[{}]",
                i18n.t(crate::MessageId::HookConsultationIgnoreAction)
            ),
            Style::default().fg(Color::DarkGray),
        ),
    ]))
    .render(chunks[3], buffer);
}

fn consultation_finding_line(i18n: &crate::I18n, model: &ConsultationCardModel) -> String {
    i18n.format(
        crate::MessageId::HookConsultationFindingLine,
        &[("finding", model.finding.as_str())],
    )
}

fn consultation_suggestion_line(i18n: &crate::I18n, model: &ConsultationCardModel) -> String {
    i18n.format(
        crate::MessageId::HookConsultationSuggestionLine,
        &[("suggestion", model.suggestion.as_str())],
    )
}

fn severity_color(severity: &str) -> Color {
    match severity.to_lowercase().as_str() {
        "critical" | "error" => Color::Red,
        "warning" | "warn" => Color::Yellow,
        "info" => Color::Cyan,
        _ => Color::White,
    }
}

fn panel_content_width(width: u16) -> usize {
    width.saturating_sub(2).max(20) as usize
}

fn wrapped_row_count(text: &str, width: usize) -> u16 {
    wrap_plain_line(text, width).len().max(1) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_consultation_card_is_finding_first() {
        let renderer = RatatuiInlineRenderer::plain_with_width(80);
        let lines = renderer.consultation_card_lines(&ConsultationCardModel {
            details_id: "hook-cmd-1-memory-pressure".to_string(),
            severity: "critical".to_string(),
            title: "Available memory is low".to_string(),
            finding: "Available memory is low and swap usage is high.".to_string(),
            suggestion: "Analyze the output before taking action.".to_string(),
        });

        let rendered = lines.join("\n");
        assert!(rendered.contains("Available memory is low [critical]:"));
        assert!(rendered.contains("Finding: Available memory is low and swap usage is high."));
        assert!(rendered.contains("Recommended action: Analyze the output before taking action."));
        assert!(!rendered.contains("Hook:"), "{rendered}");
        assert!(!rendered.contains("Confidence:"), "{rendered}");
        assert!(!rendered.contains("reason:"), "{rendered}");
        assert!(rendered.contains("[Analyze] [Ignore]"));
        assert!(!rendered.contains("[Details]"), "{rendered}");
        assert!(
            !rendered.contains("hook-cmd-1-memory-pressure"),
            "{rendered}"
        );
    }

    #[test]
    fn consultation_card_uses_zh_catalog_labels() {
        let renderer =
            RatatuiInlineRenderer::plain_with_width(80).with_language(crate::Language::ZhCn);
        let lines = renderer.consultation_card_lines(&ConsultationCardModel {
            details_id: "hook-cmd-1-memory-pressure".to_string(),
            severity: "critical".to_string(),
            title: "Available memory is low".to_string(),
            finding: "Available memory is low and swap usage is high.".to_string(),
            suggestion: "Analyze the output before taking action.".to_string(),
        });

        let rendered = lines.join("\n");
        assert!(rendered.contains("Available memory is low [critical]:"));
        assert!(rendered.contains("发现: Available memory is low and swap usage is high."));
        assert!(rendered.contains("建议动作: Analyze the output before taking action."));
        assert!(rendered.contains("[分析] [忽略]"));
        assert!(!rendered.contains("[Details]"), "{rendered}");
        assert!(
            !rendered.contains("hook-cmd-1-memory-pressure"),
            "{rendered}"
        );
        assert!(!rendered.contains("Confidence:"), "{rendered}");
        assert!(!rendered.contains("置信度:"), "{rendered}");
        assert!(!rendered.contains("[Analyze]"), "{rendered}");
    }
}
