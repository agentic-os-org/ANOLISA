use ratatui::{
    style::{Color, Modifier, Style},
    text::Span,
};

use super::super::wrap::strip_ansi_escape;

#[derive(Debug, Clone)]
pub(super) struct InlineText {
    pub(super) raw: String,
    pub(super) plain: String,
}

pub(super) fn inline_text(line: &str) -> InlineText {
    let raw = strip_ansi_escape(line);
    InlineText {
        raw: raw.clone(),
        plain: clean_inline_markdown(&raw),
    }
}

pub(super) fn clean_inline_markdown(line: &str) -> String {
    inline_segments(line)
        .into_iter()
        .map(|segment| segment.text)
        .collect::<String>()
}

pub(super) fn styled_inline_spans(text: &InlineText) -> Vec<Span<'static>> {
    inline_segments(&text.raw)
        .into_iter()
        .map(|segment| Span::styled(segment.text, ratatui_style_for_inline(segment.style)))
        .collect()
}

#[derive(Debug, Clone)]
struct InlineSegment {
    text: String,
    style: InlineStyle,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct InlineStyle {
    bold: bool,
    code: bool,
}

fn inline_segments(line: &str) -> Vec<InlineSegment> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut style = InlineStyle::default();
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '`' {
            push_inline_segment(&mut segments, &mut current, style);
            style.code = !style.code;
            continue;
        }

        if (ch == '*' || ch == '_') && chars.peek() == Some(&ch) {
            chars.next();
            push_inline_segment(&mut segments, &mut current, style);
            style.bold = !style.bold;
            continue;
        }

        current.push(ch);
    }
    push_inline_segment(&mut segments, &mut current, style);
    segments
}

fn push_inline_segment(
    segments: &mut Vec<InlineSegment>,
    current: &mut String,
    style: InlineStyle,
) {
    if current.is_empty() {
        return;
    }

    segments.push(InlineSegment {
        text: std::mem::take(current),
        style,
    });
}

fn ratatui_style_for_inline(style: InlineStyle) -> Style {
    let mut ratatui_style = Style::default();
    if style.bold {
        ratatui_style = ratatui_style.add_modifier(Modifier::BOLD);
    }
    if style.code {
        ratatui_style = ratatui_style
            .fg(Color::Yellow)
            .add_modifier(Modifier::REVERSED);
    }
    ratatui_style
}
