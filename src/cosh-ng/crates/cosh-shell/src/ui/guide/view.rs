//! Single-column guide wizard views rendered with the shared Markdown renderer.

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Paragraph, Widget, Wrap},
    Frame,
};

use crate::config::Language;
use crate::ui::{display_width, MarkdownRenderModel};

pub(crate) const MIN_COLUMNS: u16 = 40;
pub(crate) const MIN_ROWS: u16 = 16;
pub(crate) const CONTENT_WIDTH: u16 = 76;

pub(crate) enum WizardRow {
    Text(String),
    Blank,
    Choice { label: String, value: String },
}

pub(crate) struct WizardView {
    pub(crate) title: String,
    pub(crate) rows: Vec<WizardRow>,
    /// Index into `rows` of the focused choice.
    pub(crate) cursor: usize,
    pub(crate) hint: String,
    pub(crate) message: Option<String>,
    pub(crate) markdown: Option<String>,
    pub(crate) scroll: usize,
    pub(crate) language: Language,
    /// A freshly produced result scrolls its full text into view once.
    pub(crate) message_fresh: bool,
    pub(crate) follow_cursor: bool,
}

/// Rows a result may occupy before it moves into the scrollable body, where
/// the whole text stays reachable on narrow terminals.
const INLINE_MESSAGE_ROWS: usize = 2;

/// Renders the wizard and returns the effective (clamped) scroll offset so the
/// controller can store it back and keep Up/Down responsive at the boundaries.
pub(crate) fn render(frame: &mut Frame, view: &WizardView) -> usize {
    let area = frame.area();
    if area.width < MIN_COLUMNS || area.height < MIN_ROWS {
        render_too_small(frame, area, view);
        return view.scroll;
    }
    let width = area.width.saturating_sub(2).min(CONTENT_WIDTH);
    let column = Rect {
        x: area.x + (area.width - width) / 2,
        width,
        ..area
    };
    let inline_message = view.message.as_deref().and_then(|message| {
        let lines = wrap_text(message, width as usize);
        (lines.len() <= INLINE_MESSAGE_ROWS).then_some(lines)
    });
    let message_rows = inline_message.as_ref().map_or(0, |lines| lines.len()) as u16;
    let chunks = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(message_rows),
        Constraint::Length(1),
    ])
    .split(column);
    frame.render_widget(
        Paragraph::new(Line::styled(
            view.title.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        chunks[1],
    );
    let body = chunks[3];
    let scroll = match &view.markdown {
        Some(markdown) => render_markdown(frame, body, view, markdown),
        None => {
            let overflow_message = view.message.as_deref().filter(|_| inline_message.is_none());
            render_rows(frame, body, view, overflow_message)
        }
    };
    if let Some(lines) = &inline_message {
        frame.render_widget(
            Paragraph::new(
                lines
                    .iter()
                    .map(|line| {
                        Line::styled(line.clone(), Style::default().add_modifier(Modifier::BOLD))
                    })
                    .collect::<Vec<_>>(),
            ),
            chunks[4],
        );
    }
    frame.render_widget(
        Paragraph::new(Line::styled(
            view.hint.clone(),
            Style::default().add_modifier(Modifier::DIM),
        )),
        chunks[5],
    );
    scroll
}

fn render_rows(
    frame: &mut Frame,
    area: Rect,
    view: &WizardView,
    overflow_message: Option<&str>,
) -> usize {
    let width = area.width as usize;
    let height = area.height as usize;
    let mut lines: Vec<(usize, Line<'static>)> = Vec::new();
    for (index, row) in view.rows.iter().enumerate() {
        match row {
            WizardRow::Blank => lines.push((index, Line::raw(""))),
            WizardRow::Text(text) => {
                for segment in wrap_text(text, width) {
                    lines.push((index, Line::raw(segment)));
                }
            }
            WizardRow::Choice { label, value } => lines.push((
                index,
                choice_line(label, value, width, index == view.cursor),
            )),
        }
    }
    let message_owner = view.rows.len();
    if let Some(message) = overflow_message {
        lines.push((message_owner, Line::raw("")));
        for segment in wrap_text(message, width) {
            lines.push((
                message_owner,
                Line::styled(segment, Style::default().add_modifier(Modifier::BOLD)),
            ));
        }
    }
    let total = lines.len();
    let content_rows = if total <= height {
        height
    } else {
        height.saturating_sub(1)
    };
    let max_scroll = total.saturating_sub(content_rows);
    let focus_start = lines
        .iter()
        .position(|(owner, _)| *owner == view.cursor)
        .unwrap_or(0);
    let focus_end = lines
        .iter()
        .rposition(|(owner, _)| *owner == view.cursor)
        .unwrap_or(focus_start);
    let mut scroll = view.scroll.min(max_scroll);
    if view.message_fresh && overflow_message.is_some() {
        // A fresh result owns the view: show its tail rather than snapping back
        // to the focused choice, which the next keypress brings back anyway.
        scroll = max_scroll;
    } else if view.follow_cursor {
        if focus_end >= scroll + content_rows {
            scroll = (focus_end + 1).saturating_sub(content_rows).min(max_scroll);
        }
        if focus_start < scroll {
            scroll = focus_start.min(max_scroll);
        }
    }
    let visible = lines
        .into_iter()
        .skip(scroll)
        .take(content_rows)
        .map(|(_, line)| line)
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(visible),
        Rect {
            height: content_rows as u16,
            ..area
        },
    );
    if total > height {
        let indicator = Paragraph::new(Line::styled(
            format!("{}/{}", (scroll + content_rows).min(total), total),
            Style::default().add_modifier(Modifier::DIM),
        ));
        frame.render_widget(
            indicator,
            Rect::new(area.x, area.y + content_rows as u16, area.width, 1),
        );
    }
    scroll
}

fn choice_line(label: &str, value: &str, width: usize, selected: bool) -> Line<'static> {
    let prefix = if selected { "> " } else { "  " };
    let label = truncate(label, width.saturating_sub(2));
    let room = width.saturating_sub(3 + display_width(&label));
    let value = if room == 0 {
        String::new()
    } else {
        truncate(value, room)
    };
    let value_width = display_width(&value);
    let mut spans = vec![Span::raw(prefix.to_string()), Span::raw(label.clone())];
    if !value.is_empty() {
        let pad = width.saturating_sub(2 + display_width(&label) + value_width);
        spans.push(Span::raw(" ".repeat(pad.max(1))));
        spans.push(Span::raw(value));
    }
    let style = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        Style::default()
    };
    Line::from(spans).style(style)
}

fn render_markdown(frame: &mut Frame, area: Rect, view: &WizardView, markdown: &str) -> usize {
    let lines =
        MarkdownRenderModel::parse_with_language(markdown, area.width as usize, view.language)
            .spaced_styled_lines();
    let height = area.height as usize;
    let width = area.width.max(1);
    let upper = lines
        .iter()
        .map(|line| line.width().max(1))
        .sum::<usize>()
        .max(1)
        // Rect::new silently shrinks both sides once width*height passes u16::MAX,
        // which would wrap the text narrower than the buffer and overrun it.
        .min((u16::MAX / width.max(1)) as usize) as u16;
    let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
    let full = Rect::new(0, 0, width, upper);
    let mut buf = Buffer::empty(full);
    paragraph.render(full, &mut buf);
    let total = (0..upper)
        .rev()
        .find(|&y| (0..width).any(|x| buf[(x, y)].symbol() != " "))
        .map(|y| y as usize + 1)
        .unwrap_or(0);
    let content_rows = if total <= height { height } else { height - 1 };
    let scroll = if total <= height {
        0
    } else {
        view.scroll.min(total - content_rows)
    };
    let dest = frame.buffer_mut();
    for y in 0..content_rows {
        let src_y = scroll + y;
        if src_y >= total {
            break;
        }
        for x in 0..width {
            dest[(area.x + x, area.y + y as u16)] = buf[(x, src_y as u16)].clone();
        }
    }
    if total > height {
        let indicator = Paragraph::new(Line::styled(
            format!("{}/{}", (scroll + content_rows).min(total), total),
            Style::default().add_modifier(Modifier::DIM),
        ));
        frame.render_widget(
            indicator,
            Rect::new(area.x, area.y + content_rows as u16, width, 1),
        );
    }
    scroll
}

fn render_too_small(frame: &mut Frame, area: Rect, view: &WizardView) {
    let i18n = crate::I18n::new(view.language);
    let lines = vec![
        Line::raw(i18n.t(crate::MessageId::GuideWizardTooSmall)),
        Line::raw(i18n.t(crate::MessageId::GuideWizardTooSmallHint)),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn truncate(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    let target = width.saturating_sub(1);
    let mut out = String::new();
    for ch in text.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        if display_width(&candidate) > target {
            break;
        }
        out = candidate;
    }
    out.push('…');
    out
}

/// Wraps by display columns so CJK text keeps whole glyphs on narrow terminals.
fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_string()];
    }
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        let mut columns = 0usize;
        for character in paragraph.chars() {
            let glyph = character.to_string();
            let glyph_width = display_width(&glyph);
            if columns + glyph_width > width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                columns = 0;
            }
            current.push(character);
            columns += glyph_width;
        }
        lines.push(current);
    }
    lines
}

#[cfg(test)]
library_tests! {
    #[path = "view_tests.rs"]
    mod tests;
}
