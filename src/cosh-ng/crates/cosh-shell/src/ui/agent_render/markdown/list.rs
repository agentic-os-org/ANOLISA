use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Paragraph, Widget},
};

use super::super::buffer_to_lines;
use super::super::wrap::wrap_plain_line;

pub(super) fn render_ratatui_list_item(
    indent: &str,
    marker: &str,
    text: &str,
    width: usize,
) -> Vec<String> {
    let prefix = format!("{indent}{marker}");
    let rich_prefix = rich_list_prefix(indent, marker);
    let wrap_width = if ordered_marker(marker) {
        width
    } else {
        width.saturating_sub(1)
    };
    let wrapped = wrap_list_text(&prefix, text, wrap_width)
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 0 {
                line.replacen(&prefix, &rich_prefix, 1)
            } else {
                line
            }
        })
        .collect::<Vec<_>>();
    let area_width = width.max(20) as u16;
    let area = Rect::new(0, 0, area_width, wrapped.len().max(1) as u16);
    let mut buffer = Buffer::empty(area);
    let lines = wrapped
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if idx == 0 {
                styled_first_list_line(line, &rich_prefix)
            } else {
                Line::from(Span::raw(line))
            }
        })
        .collect::<Vec<_>>();

    Paragraph::new(Text::from(lines)).render(area, &mut buffer);
    buffer_to_lines(&buffer, area)
}

pub(super) fn render_plain_list_item(
    indent: &str,
    marker: &str,
    text: &str,
    width: usize,
) -> Vec<String> {
    let prefix = format!("{indent}{marker}");
    wrap_list_text(&prefix, text, width)
}

fn rich_list_prefix(indent: &str, marker: &str) -> String {
    let marker = if ordered_marker(marker) {
        marker.to_string()
    } else if indent.is_empty() {
        "• ".to_string()
    } else {
        "◦ ".to_string()
    };
    format!("{indent}{marker}")
}

fn ordered_marker(marker: &str) -> bool {
    marker
        .strip_suffix(". ")
        .is_some_and(|number| !number.is_empty() && number.chars().all(|ch| ch.is_ascii_digit()))
}

fn wrap_list_text(prefix: &str, text: &str, width: usize) -> Vec<String> {
    wrap_plain_line(&format!("{prefix}{text}"), width)
}

fn styled_first_list_line(line: String, prefix: &str) -> Line<'static> {
    let prefix_len = prefix.len().min(line.len());
    let (prefix, rest) = line.split_at(prefix_len);
    Line::from(vec![
        Span::styled(
            prefix.to_string(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(rest.to_string()),
    ])
}
