use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{block::Padding, Block, Paragraph, Widget, Wrap},
};

use super::super::buffer_to_lines;
use super::super::wrap::{char_width, display_width};
use super::highlight::styled_code_spans;

/// Styled counterpart of `render_ratatui_code_block` for the styled
/// terminal path. Builds `Line`s with styled spans directly instead of
/// flattening a `Buffer` through `buffer_to_lines`, which would drop all
/// cell styles (issue #1751: both `Span::raw` content and that flattening
/// erased syntax colors). Geometry (borders, padding, wrapping) matches
/// the unstyled renderer so panel width/height math stays identical.
pub(super) fn styled_code_block_lines(
    i18n: &crate::I18n,
    language: &str,
    lines: &[String],
    width: usize,
) -> Vec<Line<'static>> {
    let width = width.max(20);
    let content_width = width.saturating_sub(4).max(10);
    let code_lines = wrapped_code_lines(lines, content_width);
    let border_style = Style::default().fg(Color::DarkGray);
    let title = styled_code_title(i18n, language, width);
    let title_width = display_width(&title);
    let mut rendered = Vec::new();

    let top_fill = width.saturating_sub(title_width + 2);
    rendered.push(Line::from(vec![
        Span::styled("┌".to_string(), border_style),
        Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(format!("{}┐", "─".repeat(top_fill)), border_style),
    ]));

    let content_rows = if code_lines.is_empty() {
        vec![String::new()]
    } else {
        code_lines
    };
    for line in content_rows {
        let fill = content_width.saturating_sub(display_width(&line));
        let mut spans = vec![Span::styled("│ ".to_string(), border_style)];
        spans.extend(styled_code_spans(language, &line));
        spans.push(Span::raw(" ".repeat(fill)));
        spans.push(Span::styled(" │".to_string(), border_style));
        rendered.push(Line::from(spans));
    }

    rendered.push(Line::from(Span::styled(
        format!("└{}┘", "─".repeat(width.saturating_sub(2))),
        border_style,
    )));
    rendered
}

pub(super) fn render_ratatui_code_block(
    i18n: &crate::I18n,
    language: &str,
    lines: &[String],
    width: usize,
) -> Vec<String> {
    let width = width.max(20) as u16;
    let content_width = width.saturating_sub(4).max(10) as usize;
    let code_lines = wrapped_code_lines(lines, content_width);
    let height = code_lines.len().max(1) as u16 + 2;
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    let title = code_block_title(i18n, language);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Line::from(Span::styled(
            format!(" {title} "),
            Style::default().add_modifier(Modifier::BOLD),
        )))
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    block.render(area, &mut buffer);

    let text = if code_lines.is_empty() {
        Text::from(Line::from(""))
    } else {
        Text::from(
            code_lines
                .into_iter()
                .map(|line| Line::from(Span::raw(line)))
                .collect::<Vec<_>>(),
        )
    };
    Paragraph::new(text)
        .wrap(Wrap { trim: false })
        .render(inner, &mut buffer);

    buffer_to_lines(&buffer, area)
}

pub(super) fn render_plain_code_block(
    i18n: &crate::I18n,
    language: &str,
    lines: &[String],
    width: usize,
) -> Vec<String> {
    let label = code_block_title(i18n, language);
    let mut rendered = vec![format!("+-- {label}")];
    let content_width = width.saturating_sub(2).max(10);
    let code_lines = wrapped_code_lines(lines, content_width);
    if code_lines.is_empty() {
        rendered.push("|".to_string());
    } else {
        for line in code_lines {
            rendered.push(format!("| {line}"));
        }
    }
    rendered.push("+--".to_string());
    rendered
}

fn wrapped_code_lines(lines: &[String], width: usize) -> Vec<String> {
    lines
        .iter()
        .flat_map(|line| wrap_code_line(line, width))
        .collect()
}

fn code_block_title(i18n: &crate::I18n, language: &str) -> String {
    if language.is_empty() {
        i18n.t(crate::MessageId::MarkdownCodeLabel).to_string()
    } else {
        i18n.format(
            crate::MessageId::MarkdownCodeWithLanguageLabel,
            &[("language", language)],
        )
    }
}

/// Title for the styled border, truncated to the block width so an
/// over-long fenced-code info string cannot produce an over-wide `Line`
/// that the outer `Paragraph` re-wraps into broken borders. The unstyled
/// path gets this clipping for free from its fixed-width `Buffer`.
fn styled_code_title(i18n: &crate::I18n, language: &str, width: usize) -> String {
    let title = format!(" {} ", code_block_title(i18n, language));
    // Keep room for both corner glyphs.
    let max_width = width.saturating_sub(2);
    if display_width(&title) <= max_width {
        return title;
    }
    let target = max_width.saturating_sub(2).max(1);
    let mut clipped = String::new();
    for ch in title.trim_end().chars() {
        if display_width(&clipped) + char_width(ch) > target {
            break;
        }
        clipped.push(ch);
    }
    format!("{clipped}… ")
}

fn wrap_code_line(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    if line.is_empty() {
        return vec![String::new()];
    }

    let indent = leading_whitespace(line);
    let continuation_indent = if display_width(indent) >= width {
        ""
    } else {
        indent
    };
    wrap_code_segment(line, continuation_indent, width)
}

fn leading_whitespace(line: &str) -> &str {
    let end = line
        .char_indices()
        .take_while(|(_, ch)| ch.is_whitespace())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .last()
        .unwrap_or(0);
    &line[..end]
}

fn wrap_code_segment(line: &str, continuation_indent: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut current_width = 0;

    for ch in line.chars() {
        let ch_width = char_width(ch);
        if ch_width > 0 && current_width + ch_width > width && !current.is_empty() {
            lines.push(current);
            current = continuation_indent.to_string();
            current_width = display_width(continuation_indent);
        }
        current.push(ch);
        current_width += ch_width;
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(current);
    }
    lines
}
