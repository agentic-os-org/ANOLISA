use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{block::Padding, Block, Borders, Paragraph, Widget},
};

use super::super::buffer_to_lines;
use super::super::wrap::{display_width, wrap_plain_line};

pub(super) fn render_ratatui_heading(level: usize, text: &str, width: usize) -> Vec<String> {
    let width = width.max(20) as u16;
    let heading_lines = wrap_plain_line(text, width.saturating_sub(2) as usize);
    let mut rendered = Vec::new();
    for line in heading_lines {
        let area = Rect::new(0, 0, width, 2);
        let mut buffer = Buffer::empty(area);
        let style = if level <= 1 {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD)
        };
        let block = Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::DarkGray));
        let inner = block.inner(area);
        block.render(area, &mut buffer);
        Paragraph::new(Line::from(Span::styled(line, style))).render(inner, &mut buffer);
        rendered.extend(buffer_to_lines(&buffer, area));
    }
    rendered
}

pub(super) fn render_plain_heading(level: usize, text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for line in wrap_plain_line(text, width) {
        let underline = if level <= 1 { '=' } else { '-' };
        let underline_width = display_width(&line).min(width).max(3);
        lines.push(line);
        lines.push(underline.to_string().repeat(underline_width));
    }
    lines
}

pub(super) fn render_ratatui_quote(text: &str, width: usize) -> Vec<String> {
    let width = width.max(20) as u16;
    let content_width = width.saturating_sub(3).max(10) as usize;
    let quote_lines = wrap_plain_line(text, content_width);
    let height = quote_lines.len().max(1) as u16;
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    let block = Block::default()
        .borders(Borders::LEFT)
        .padding(Padding::horizontal(1))
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    block.render(area, &mut buffer);

    let text = Text::from(
        quote_lines
            .into_iter()
            .map(|line| Line::from(Span::styled(line, Style::default().fg(Color::Gray))))
            .collect::<Vec<_>>(),
    );
    Paragraph::new(text).render(inner, &mut buffer);
    buffer_to_lines(&buffer, area)
}

pub(super) fn render_plain_quote(text: &str, width: usize) -> Vec<String> {
    let content_width = width.saturating_sub(2).max(10);
    wrap_plain_line(text, content_width)
        .into_iter()
        .map(|line| format!("> {line}"))
        .collect()
}
