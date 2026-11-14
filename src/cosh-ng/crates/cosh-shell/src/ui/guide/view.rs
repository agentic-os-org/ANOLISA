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
}

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
    let message_rows = u16::from(view.message.is_some());
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
            render_rows(frame, body, view);
            view.scroll
        }
    };
    if let Some(message) = &view.message {
        frame.render_widget(
            Paragraph::new(Line::styled(
                message.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )),
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

fn render_rows(frame: &mut Frame, area: Rect, view: &WizardView) {
    let width = area.width as usize;
    let height = area.height as usize;
    let total = view.rows.len();
    let lines = view
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| match row {
            WizardRow::Blank => Line::raw(""),
            WizardRow::Text(text) => Line::raw(truncate(text, width)),
            WizardRow::Choice { label, value } => {
                choice_line(label, value, width, index == view.cursor)
            }
        })
        .collect::<Vec<_>>();
    let content_rows = if total <= height {
        height
    } else {
        height.saturating_sub(1)
    };
    let max_scroll = total.saturating_sub(content_rows);
    let scroll = view
        .cursor
        .saturating_sub(content_rows.saturating_sub(1))
        .min(max_scroll);
    let visible = lines
        .into_iter()
        .skip(scroll)
        .take(content_rows)
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
}

fn choice_line(label: &str, value: &str, width: usize, selected: bool) -> Line<'static> {
    let prefix = if selected { "> " } else { "  " };
    let value_width = display_width(value);
    let room = width.saturating_sub(2 + value_width + usize::from(!value.is_empty()));
    let label = truncate(label, room);
    let mut spans = vec![Span::raw(prefix.to_string()), Span::raw(label.clone())];
    if !value.is_empty() {
        let pad = width.saturating_sub(2 + display_width(&label) + value_width);
        spans.push(Span::raw(" ".repeat(pad.max(1))));
        spans.push(Span::raw(value.to_string()));
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
            .styled_lines();
    let height = area.height as usize;
    let width = area.width.max(1);
    let upper = lines
        .iter()
        .map(|line| line.width().max(1))
        .sum::<usize>()
        .max(1)
        .min(u16::MAX as usize) as u16;
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

#[cfg(test)]
library_tests! {
    #[path = "view_tests.rs"]
    mod tests;
}
