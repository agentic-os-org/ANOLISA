use std::io::{self, Write};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border::ROUNDED,
    text::{Line, Span, Text},
    widgets::{Block, Paragraph, Widget, Wrap},
};

use super::{
    buffer_to_lines, buffer_to_styled_lines, display_width, wrap_plain_line, RatatuiInlineRenderer,
};

const RECOMMENDATION_FOOTER_PREFIX: &str = "  ";

#[derive(Debug, Clone)]
pub struct RecommendationPanelModel<'a> {
    pub title: &'a str,
    pub summary: Option<&'a str>,
    pub commands: &'a [String],
}

#[derive(Debug, Clone)]
pub struct RecommendationActionPanelModel<'a> {
    pub title: &'a str,
    pub primary: String,
    pub command: Option<&'a str>,
    pub message: &'a str,
}

impl RatatuiInlineRenderer {
    pub fn write_recommendation_panel<W: Write>(
        &self,
        output: &mut W,
        model: RecommendationPanelModel<'_>,
    ) -> io::Result<usize> {
        let lines = self.recommendation_panel_write_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub fn recommendation_panel_lines(&self, model: RecommendationPanelModel<'_>) -> Vec<String> {
        if self.plain {
            return self.plain_recommendation_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let i18n = self.i18n();
        let height = recommendation_panel_height(&model, i18n, width);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_recommendation_panel(model, i18n, area, &mut buffer);
        buffer_to_lines(&buffer, area)
    }

    fn recommendation_panel_write_lines(&self, model: RecommendationPanelModel<'_>) -> Vec<String> {
        if self.plain {
            return self.plain_recommendation_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let i18n = self.i18n();
        let height = recommendation_panel_height(&model, i18n, width);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_recommendation_panel(model, i18n, area, &mut buffer);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }

    fn plain_recommendation_panel_lines(&self, model: RecommendationPanelModel<'_>) -> Vec<String> {
        let width = self.panel_standard_width();
        let content_width = panel_content_width(width);
        let i18n = self.i18n();
        let mut lines = vec![format!("{}:", model.title)];
        if let Some(summary) = model.summary {
            lines.extend(wrap_prefixed_recommendation_line(
                "  ",
                summary,
                content_width,
            ));
        }
        if model.commands.is_empty() {
            lines.push(format!(
                "  {}",
                i18n.t(crate::MessageId::RecommendationEmptyBody)
            ));
        } else {
            lines.extend(
                model
                    .commands
                    .iter()
                    .enumerate()
                    .flat_map(|(idx, command)| {
                        wrap_prefixed_recommendation_line(
                            &format!("  {}. ", idx + 1),
                            command,
                            content_width,
                        )
                    }),
            );
        }
        lines.extend(wrap_prefixed_recommendation_line(
            RECOMMENDATION_FOOTER_PREFIX,
            i18n.t(crate::MessageId::RecommendationFooter),
            content_width,
        ));
        lines
    }

    pub fn write_recommendation_action_panel<W: Write>(
        &self,
        output: &mut W,
        model: RecommendationActionPanelModel<'_>,
    ) -> io::Result<usize> {
        let lines = self.recommendation_action_panel_write_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub fn recommendation_action_panel_lines(
        &self,
        model: RecommendationActionPanelModel<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_recommendation_action_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let height = recommendation_action_panel_height(&model, width);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_recommendation_action_panel(model, area, &mut buffer);
        buffer_to_lines(&buffer, area)
    }

    fn recommendation_action_panel_write_lines(
        &self,
        model: RecommendationActionPanelModel<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_recommendation_action_panel_lines(model);
        }

        let width = self.panel_standard_width();
        let height = recommendation_action_panel_height(&model, width);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_recommendation_action_panel(model, area, &mut buffer);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }

    fn plain_recommendation_action_panel_lines(
        &self,
        model: RecommendationActionPanelModel<'_>,
    ) -> Vec<String> {
        let width = self.panel_standard_width();
        let content_width = panel_content_width(width);
        let mut lines = vec![format!("{}:", model.title)];
        lines.extend(wrap_plain_line(&model.primary, content_width));
        if let Some(command) = model.command {
            lines.extend(wrap_prefixed_recommendation_line(
                "  ",
                command,
                content_width,
            ));
        }
        lines.extend(wrap_plain_line(model.message, content_width));
        lines
    }
}

fn recommendation_panel_height(
    model: &RecommendationPanelModel<'_>,
    i18n: crate::I18n,
    width: u16,
) -> u16 {
    let content_width = panel_content_width(width);
    recommendation_summary_rows(model, content_width)
        + recommendation_command_rows(model, content_width)
        + recommendation_footer_rows(i18n, content_width)
        + 2
}

fn render_recommendation_panel(
    model: RecommendationPanelModel<'_>,
    i18n: crate::I18n,
    area: Rect,
    buffer: &mut Buffer,
) {
    let block = Block::bordered()
        .title(Line::from(Span::styled(
            format!("─ {} ", model.title),
            Style::default().add_modifier(Modifier::BOLD),
        )))
        .border_set(ROUNDED)
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(area);
    block.render(area, buffer);

    let content_width = inner.width as usize;
    let summary_rows = recommendation_summary_rows(&model, content_width);
    let command_rows = recommendation_command_rows(&model, content_width);
    let footer_rows = recommendation_footer_rows(i18n, content_width);
    let chunks = Layout::vertical(vec![
        Constraint::Length(summary_rows),
        Constraint::Length(command_rows),
        Constraint::Length(footer_rows),
    ])
    .split(inner);

    let command_lines = if model.commands.is_empty() {
        vec![Line::from(format!(
            "  {}",
            i18n.t(crate::MessageId::RecommendationEmptyBody)
        ))]
    } else {
        model
            .commands
            .iter()
            .enumerate()
            .map(|(idx, command)| {
                Line::from(vec![
                    Span::styled(
                        format!("  {}. ", idx + 1),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(command.to_string()),
                ])
            })
            .collect::<Vec<_>>()
    };
    if let Some(summary) = model.summary {
        let summary_lines = wrap_prefixed_recommendation_line("  ", summary, content_width)
            .into_iter()
            .map(Line::from)
            .collect::<Vec<_>>();
        Paragraph::new(Text::from(summary_lines))
            .wrap(Wrap { trim: false })
            .render(chunks[0], buffer);
    }

    Paragraph::new(Text::from(command_lines))
        .wrap(Wrap { trim: false })
        .render(chunks[1], buffer);

    Paragraph::new(Text::from(Line::from(vec![
        Span::raw(RECOMMENDATION_FOOTER_PREFIX),
        Span::raw(i18n.t(crate::MessageId::RecommendationFooter)),
    ])))
    .wrap(Wrap { trim: false })
    .render(chunks[2], buffer);
}

fn recommendation_action_panel_height(
    model: &RecommendationActionPanelModel<'_>,
    width: u16,
) -> u16 {
    let content_width = panel_content_width(width);
    let primary_rows = wrapped_row_count(&model.primary, content_width);
    let command_rows = model
        .command
        .map(|command| wrapped_row_count(&format!("  {command}"), content_width))
        .unwrap_or(0);
    let message_rows = wrapped_row_count(model.message, content_width);
    primary_rows + command_rows + message_rows + 2
}

fn render_recommendation_action_panel(
    model: RecommendationActionPanelModel<'_>,
    area: Rect,
    buffer: &mut Buffer,
) {
    let block = Block::bordered()
        .title(Line::from(Span::styled(
            format!("─ {} ", model.title),
            Style::default().add_modifier(Modifier::BOLD),
        )))
        .border_set(ROUNDED)
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(area);
    block.render(area, buffer);

    let content_width = inner.width as usize;
    let primary_rows = wrapped_row_count(&model.primary, content_width);
    let command_rows = model
        .command
        .map(|command| wrapped_row_count(&format!("  {command}"), content_width))
        .unwrap_or(0);
    let message_rows = wrapped_row_count(model.message, content_width);
    let chunks = Layout::vertical(vec![
        Constraint::Length(primary_rows),
        Constraint::Length(command_rows),
        Constraint::Length(message_rows),
    ])
    .split(inner);

    Paragraph::new(model.primary)
        .wrap(Wrap { trim: true })
        .render(chunks[0], buffer);

    if let Some(command) = model.command {
        Paragraph::new(format!("  {command}"))
            .wrap(Wrap { trim: true })
            .render(chunks[1], buffer);
    }

    Paragraph::new(model.message)
        .wrap(Wrap { trim: true })
        .render(chunks[2], buffer);
}

fn recommendation_command_rows(model: &RecommendationPanelModel<'_>, width: usize) -> u16 {
    if model.commands.is_empty() {
        return 1;
    }

    model
        .commands
        .iter()
        .enumerate()
        .map(|(idx, command)| wrapped_row_count(&format!("  {}. {command}", idx + 1), width))
        .sum()
}

fn recommendation_summary_rows(model: &RecommendationPanelModel<'_>, width: usize) -> u16 {
    model
        .summary
        .map(|summary| wrap_prefixed_recommendation_line("  ", summary, width).len() as u16)
        .unwrap_or(0)
}

fn recommendation_footer_rows(i18n: crate::I18n, width: usize) -> u16 {
    wrapped_row_count(
        &format!(
            "{}{}",
            RECOMMENDATION_FOOTER_PREFIX,
            i18n.t(crate::MessageId::RecommendationFooter)
        ),
        width,
    )
}

fn panel_content_width(width: u16) -> usize {
    width.saturating_sub(2).max(20) as usize
}

fn wrapped_row_count(text: &str, width: usize) -> u16 {
    wrap_plain_line(text, width).len().max(1) as u16
}

fn wrap_prefixed_recommendation_line(prefix: &str, text: &str, width: usize) -> Vec<String> {
    let continuation = " ".repeat(display_width(prefix));
    let content_width = width.saturating_sub(display_width(prefix)).max(1);
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
