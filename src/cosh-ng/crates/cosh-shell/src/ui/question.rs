use std::io::{self, Write};

use crate::{
    question::choices::{
        question_choice_count as shared_question_choice_count, question_custom_answer_index,
    },
    types::QuestionSelectionMode,
};

use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::border::ROUNDED,
    text::{Line, Span, Text},
    widgets::{block::Padding, Block, Paragraph, Widget, Wrap},
};

use crate::ui::agent_render::{
    buffer_to_lines, buffer_to_styled_lines, display_width, wrap_plain_line,
    wrap_plain_line_with_prefix, RatatuiInlineRenderer,
};

#[path = "question/cursor.rs"]
mod cursor;
pub use cursor::QuestionCursorPlacement;
#[path = "question/presentation.rs"]
mod presentation;
pub use presentation::QuestionPanelPresentation;
use presentation::{
    instruction_rows as presented_instruction_rows, instruction_text as presented_instruction_text,
};
#[path = "question/styles.rs"]
mod styles;
use styles::{option_heading_style, option_marker_style, render_custom_option_lines};

/// Separates an option label from secondary text rendered in dark gray.
pub(crate) const OPTION_DETAIL_SEPARATOR: &str = "\n    ";

#[derive(Debug, Clone)]
pub struct QuestionPanelModel<'a> {
    pub id: &'a str,
    pub question: &'a str,
    pub options: &'a [String],
    pub selected_option: usize,
    pub selected_options: &'a [usize],
    pub custom_answer: &'a str,
    pub allow_free_text: bool,
    pub selection_mode: QuestionSelectionMode,
    pub input_feedback: QuestionInputFeedback,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuestionInputFeedback {
    Disabled,
    #[default]
    None,
    Required,
    Invalid,
    SelectionRequired,
}

#[derive(Debug, Clone)]
pub struct QuestionAnswerPanelModel<'a> {
    pub id: &'a str,
    pub question: &'a str,
    pub answer: &'a str,
    pub message: &'a str,
}

impl RatatuiInlineRenderer {
    pub fn write_question_panel<W: Write>(
        &self,
        output: &mut W,
        model: QuestionPanelModel<'_>,
    ) -> io::Result<usize> {
        self.write_question_panel_with_presentation(output, model, Default::default())
    }

    pub(crate) fn write_question_panel_with_presentation<W: Write>(
        &self,
        output: &mut W,
        model: QuestionPanelModel<'_>,
        presentation: QuestionPanelPresentation<'_>,
    ) -> io::Result<usize> {
        let cursor = self.question_cursor_placement(&model);
        let lines = self.question_panel_write_lines_with_presentation(model, presentation);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        if self.supports_question_cursor() {
            if let Some(cursor) = cursor {
                let up = lines.len().saturating_sub(cursor.row);
                write!(output, "\x1b[{up}A\r")?;
                if cursor.column > 0 {
                    write!(output, "\x1b[{}C", cursor.column)?;
                }
                write!(output, "\x1b[?25h")?;
            }
        }
        Ok(lines.len())
    }

    pub fn question_panel_lines(&self, model: QuestionPanelModel<'_>) -> Vec<String> {
        self.question_panel_lines_with_presentation(model, Default::default())
    }

    pub(crate) fn question_panel_lines_with_presentation(
        &self,
        model: QuestionPanelModel<'_>,
        presentation: QuestionPanelPresentation<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_question_panel_lines(model, presentation);
        }

        let width = self.panel_standard_width();
        let i18n = self.i18n();
        let height = question_panel_height(&model, i18n, width, presentation);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_question_panel(model, i18n, presentation, area, &mut buffer);
        buffer_to_lines(&buffer, area)
    }

    fn question_panel_write_lines_with_presentation(
        &self,
        model: QuestionPanelModel<'_>,
        presentation: QuestionPanelPresentation<'_>,
    ) -> Vec<String> {
        if self.plain {
            return self.plain_question_panel_lines(model, presentation);
        }

        let width = self.panel_standard_width();
        let i18n = self.i18n();
        let height = question_panel_height(&model, i18n, width, presentation);
        let area = Rect::new(0, 0, width, height);
        let mut buffer = Buffer::empty(area);
        render_question_panel(model, i18n, presentation, area, &mut buffer);
        if self.styles_enabled() {
            buffer_to_styled_lines(&buffer, area)
        } else {
            buffer_to_lines(&buffer, area)
        }
    }

    fn plain_question_panel_lines(
        &self,
        model: QuestionPanelModel<'_>,
        presentation: QuestionPanelPresentation<'_>,
    ) -> Vec<String> {
        let width = self.panel_standard_width();
        let content_width = question_content_width(width);
        let i18n = self.i18n();
        let mut lines = vec![presentation
            .title
            .unwrap_or_else(|| i18n.t(crate::MessageId::QuestionTitle))
            .to_string()];
        lines.extend(wrap_plain_line(model.question, content_width));
        if !model.options.is_empty() {
            let selected = selected_option(&model);
            lines.push(question_option_heading(&model, i18n).to_string());
            lines.extend(
                model.options.iter().enumerate().flat_map(|(idx, option)| {
                    plain_option_lines(&model, idx, option, content_width)
                }),
            );
            if let Some(idx) =
                question_custom_answer_index(model.options.len(), model.allow_free_text)
            {
                let marker = if selected == idx { "> " } else { "  " };
                let prefix = format!("{marker}[{}] ", idx + 1);
                let label = custom_option_visual_label(
                    i18n,
                    model.custom_answer,
                    model.input_feedback,
                    selected == idx,
                    true,
                );
                lines.extend(wrap_option_text(&prefix, &label, content_width));
            }
            lines.extend(wrap_plain_line(
                &presented_instruction_text(&model, i18n, selected, presentation, true),
                content_width,
            ));
        } else {
            lines.extend(free_text_answer_lines(&model, i18n, content_width));
            lines.extend(wrap_plain_line(
                &presented_instruction_text(
                    &model,
                    i18n,
                    selected_option(&model),
                    presentation,
                    true,
                ),
                content_width,
            ));
        }
        lines
    }

    pub fn write_question_answer_panel<W: Write>(
        &self,
        output: &mut W,
        model: QuestionAnswerPanelModel<'_>,
    ) -> io::Result<usize> {
        let lines = self.question_answer_panel_write_lines(model);
        for line in &lines {
            writeln!(output, "{line}")?;
        }
        Ok(lines.len())
    }

    pub fn question_answer_panel_lines(&self, model: QuestionAnswerPanelModel<'_>) -> Vec<String> {
        let width = self.panel_standard_width();
        self.question_answer_lines(model, width, false)
    }

    fn question_answer_panel_write_lines(
        &self,
        model: QuestionAnswerPanelModel<'_>,
    ) -> Vec<String> {
        let width = self.panel_standard_width();
        self.question_answer_lines(model, width, self.styles_enabled())
    }

    fn question_answer_lines(
        &self,
        model: QuestionAnswerPanelModel<'_>,
        width: u16,
        styled: bool,
    ) -> Vec<String> {
        let content_width = question_content_width(width);
        let i18n = self.i18n();
        let lines = wrapped_question_label_rows(
            i18n.t(crate::MessageId::QuestionAnswerLabel),
            model.answer,
            content_width,
        );
        if !styled {
            return lines;
        }

        let area = Rect::new(0, 0, width, lines.len() as u16);
        let mut buffer = Buffer::empty(area);
        let styled_lines = lines
            .into_iter()
            .map(|line| {
                Line::from(Span::styled(
                    line,
                    Style::default()
                        .fg(Color::Green)
                        .add_modifier(Modifier::BOLD),
                ))
            })
            .collect::<Vec<_>>();
        Paragraph::new(Text::from(styled_lines)).render(area, &mut buffer);
        buffer_to_styled_lines(&buffer, area)
    }
}

fn question_panel_height(
    model: &QuestionPanelModel<'_>,
    i18n: crate::I18n,
    width: u16,
    presentation: QuestionPanelPresentation<'_>,
) -> u16 {
    let content_width = question_content_width(width);
    question_rows(model, content_width)
        + option_rows(model, i18n, content_width)
        + presented_instruction_rows(model, i18n, content_width, presentation)
        + 2
}

fn render_question_panel(
    model: QuestionPanelModel<'_>,
    i18n: crate::I18n,
    presentation: QuestionPanelPresentation<'_>,
    area: Rect,
    buffer: &mut Buffer,
) {
    let selected_option = selected_option(&model);
    let block = Block::bordered()
        .padding(Padding::horizontal(1))
        .title(Line::from(Span::styled(
            format!(
                " {} ",
                presentation
                    .title
                    .unwrap_or_else(|| i18n.t(crate::MessageId::QuestionTitle))
            ),
            Style::default().add_modifier(Modifier::BOLD),
        )))
        .border_set(ROUNDED)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    block.render(area, buffer);

    let content_width = inner.width as usize;
    let question_rows = question_rows(&model, content_width);
    let option_rows = option_rows(&model, i18n, content_width);
    let instruction_rows = presented_instruction_rows(&model, i18n, content_width, presentation);
    let chunks = Layout::vertical(vec![
        Constraint::Length(question_rows),
        Constraint::Length(option_rows),
        Constraint::Length(instruction_rows),
    ])
    .split(inner);

    Paragraph::new(model.question.to_string())
        .wrap(Wrap { trim: true })
        .render(chunks[0], buffer);

    let mut option_lines = Vec::new();
    if model.options.is_empty() {
        option_lines.extend(free_text_answer_render_lines(&model, i18n, content_width));
    } else {
        option_lines.push(Line::from(Span::styled(
            question_option_heading(&model, i18n).to_string(),
            option_heading_style(model.input_feedback),
        )));
        option_lines.extend(rendered_option_lines(
            &model,
            selected_option,
            content_width,
        ));
        if let Some(idx) = question_custom_answer_index(model.options.len(), model.allow_free_text)
        {
            let selected = idx == selected_option;
            option_lines.extend(render_custom_option_lines(
                idx,
                selected,
                i18n,
                model.custom_answer,
                model.input_feedback,
                content_width,
            ));
        }
    }
    if option_rows > 0 {
        Paragraph::new(Text::from(option_lines))
            .wrap(Wrap { trim: false })
            .render(chunks[1], buffer);
    }

    let instruction =
        presented_instruction_text(&model, i18n, selected_option, presentation, false);
    Paragraph::new(Line::from(vec![
        Span::styled(
            presentation
                .keys_prefix
                .unwrap_or_else(|| i18n.t(crate::MessageId::QuestionKeysPrefix)),
            Style::default().fg(Color::DarkGray),
        ),
        Span::raw(instruction),
    ]))
    .wrap(Wrap { trim: true })
    .render(chunks[2], buffer);
}

fn selected_option(model: &QuestionPanelModel<'_>) -> usize {
    model
        .selected_option
        .min(question_choice_count(model).saturating_sub(1))
}

fn question_content_width(width: u16) -> usize {
    width.saturating_sub(4).max(20) as usize
}

fn question_rows(model: &QuestionPanelModel<'_>, width: usize) -> u16 {
    wrapped_row_count(model.question, width)
}

fn option_rows(model: &QuestionPanelModel<'_>, i18n: crate::I18n, width: usize) -> u16 {
    if model.options.is_empty() {
        return free_text_answer_lines(model, i18n, width).len() as u16;
    }
    let option_count = model
        .options
        .iter()
        .enumerate()
        .map(|(idx, option)| wrapped_row_count(&option_line_text(model, idx, option), width))
        .sum::<u16>()
        + question_custom_answer_index(model.options.len(), model.allow_free_text)
            .map(|idx| wrapped_row_count(&custom_option_text(model, idx, i18n), width))
            .unwrap_or(0);
    1 + option_count
}

fn wrapped_row_count(text: &str, width: usize) -> u16 {
    wrap_plain_line(text, width).len().max(1) as u16
}

fn question_choice_count(model: &QuestionPanelModel<'_>) -> usize {
    shared_question_choice_count(model.options.len(), model.allow_free_text)
}

fn plain_option_lines(
    model: &QuestionPanelModel<'_>,
    idx: usize,
    option: &str,
    width: usize,
) -> Vec<String> {
    let marker = if selected_option(model) == idx {
        "> "
    } else {
        "  "
    };
    let prefix = if model.selection_mode == QuestionSelectionMode::Multiple {
        let checkbox = if option_is_checked(model, idx) {
            "x"
        } else {
            " "
        };
        format!("{marker}[{checkbox}] [{}] ", idx + 1)
    } else {
        format!("{marker}[{}] ", idx + 1)
    };
    wrap_option_text(&prefix, option, width)
}

fn option_line_text(model: &QuestionPanelModel<'_>, idx: usize, option: &str) -> String {
    let marker = if selected_option(model) == idx {
        "> "
    } else {
        "  "
    };
    if model.selection_mode == QuestionSelectionMode::Multiple {
        let checkbox = if option_is_checked(model, idx) {
            "x"
        } else {
            " "
        };
        format!("{marker}[{checkbox}] [{}] {}", idx + 1, option)
    } else {
        format!("{marker}[{}] {}", idx + 1, option)
    }
}

fn question_option_heading(model: &QuestionPanelModel<'_>, i18n: crate::I18n) -> &'static str {
    if model.options.is_empty() {
        i18n.t(crate::MessageId::QuestionAnswerLabel)
    } else if model.input_feedback == QuestionInputFeedback::SelectionRequired {
        if model.allow_free_text {
            i18n.t(crate::MessageId::QuestionSelectionRequiredWithText)
        } else {
            i18n.t(crate::MessageId::QuestionSelectionRequired)
        }
    } else if model.selection_mode == QuestionSelectionMode::Multiple {
        i18n.t(crate::MessageId::QuestionSelectMultipleLabel)
    } else {
        i18n.t(crate::MessageId::QuestionSelectOneLabel)
    }
}

fn rendered_option_lines(
    model: &QuestionPanelModel<'_>,
    selected_option: usize,
    width: usize,
) -> Vec<Line<'static>> {
    model
        .options
        .iter()
        .enumerate()
        .flat_map(|(idx, option)| {
            let selected = idx == selected_option;
            let checked = option_is_checked(model, idx);
            let marker = if selected { ">" } else { " " };
            let prefix = if model.selection_mode == QuestionSelectionMode::Multiple {
                let checkbox = if checked { "x" } else { " " };
                format!("{marker} [{checkbox}] [{}] ", idx + 1)
            } else {
                format!("{marker} [{}] ", idx + 1)
            };
            render_wrapped_option(&prefix, option, option_marker_style(selected), width)
        })
        .collect()
}

fn render_wrapped_option(
    prefix: &str,
    text: &str,
    prefix_style: Style,
    width: usize,
) -> Vec<Line<'static>> {
    let detail_start = text
        .split_once(OPTION_DETAIL_SEPARATOR)
        .map(|(label, _)| wrap_option_text(prefix, label, width).len());
    let detail_style = Style::default().fg(Color::DarkGray);
    wrap_option_text(prefix, text, width)
        .into_iter()
        .enumerate()
        .map(|(idx, line)| {
            if detail_start.is_some_and(|start| idx >= start) {
                Line::from(Span::styled(line, detail_style))
            } else if idx == 0 {
                let prefix_len = prefix.len().min(line.len());
                let (prefix, rest) = line.split_at(prefix_len);
                Line::from(vec![
                    Span::styled(prefix.to_string(), prefix_style),
                    Span::raw(rest.to_string()),
                ])
            } else {
                Line::from(Span::raw(line))
            }
        })
        .collect()
}

fn custom_option_text(model: &QuestionPanelModel<'_>, idx: usize, i18n: crate::I18n) -> String {
    format!(
        "  [{}] {}",
        idx + 1,
        custom_option_visual_label(
            i18n,
            model.custom_answer,
            model.input_feedback,
            selected_option(model) == idx,
            false,
        )
    )
}

fn custom_option_label(i18n: crate::I18n, custom_answer: &str) -> String {
    if custom_answer.trim().is_empty() {
        i18n.t(crate::MessageId::QuestionOtherEmptyLabel)
            .to_string()
    } else {
        format!(
            "{}: {}",
            i18n.t(crate::MessageId::QuestionAnswerLabel),
            custom_answer
        )
    }
}

fn custom_option_visual_label(
    i18n: crate::I18n,
    custom_answer: &str,
    feedback: QuestionInputFeedback,
    active: bool,
    plain: bool,
) -> String {
    let label = custom_option_label(i18n, custom_answer);
    if !active || !custom_answer.trim().is_empty() || feedback == QuestionInputFeedback::Disabled {
        return label;
    }
    let ghost = match feedback {
        QuestionInputFeedback::Required => i18n.t(crate::MessageId::QuestionRequiredGhost),
        QuestionInputFeedback::Invalid => i18n.t(crate::MessageId::QuestionInvalidGhost),
        _ => i18n.t(crate::MessageId::QuestionDefaultGhost),
    };
    if plain {
        format!("{label}  [{ghost}]")
    } else {
        format!("{label}  › {ghost}")
    }
}

fn question_input_text(model: &QuestionPanelModel<'_>, i18n: crate::I18n, plain: bool) -> String {
    if !model.custom_answer.trim().is_empty() {
        return model.custom_answer.to_string();
    }
    let text = match model.input_feedback {
        QuestionInputFeedback::Required => i18n.t(crate::MessageId::QuestionRequiredGhost),
        QuestionInputFeedback::Invalid => i18n.t(crate::MessageId::QuestionInvalidGhost),
        _ => i18n.t(crate::MessageId::QuestionDefaultGhost),
    };
    if plain {
        format!("[{text}]")
    } else {
        format!("  › {text}")
    }
}

fn wrapped_question_label_rows(label: &str, text: &str, width: usize) -> Vec<String> {
    let prefix = format!("{label}: ");
    let continuation = " ".repeat(display_width(&prefix));
    let content_width = width.saturating_sub(display_width(&prefix)).max(1);
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

fn free_text_answer_lines(
    model: &QuestionPanelModel<'_>,
    i18n: crate::I18n,
    width: usize,
) -> Vec<String> {
    if model.options.is_empty()
        && model.allow_free_text
        && model.input_feedback != QuestionInputFeedback::Disabled
    {
        wrapped_question_label_rows(
            i18n.t(crate::MessageId::QuestionAnswerLabel),
            &question_input_text(model, i18n, true),
            width,
        )
    } else {
        Vec::new()
    }
}

fn free_text_answer_render_lines(
    model: &QuestionPanelModel<'_>,
    i18n: crate::I18n,
    width: usize,
) -> Vec<Line<'static>> {
    if !model.options.is_empty()
        || !model.allow_free_text
        || model.input_feedback == QuestionInputFeedback::Disabled
    {
        return Vec::new();
    }
    let rows = wrapped_question_label_rows(
        i18n.t(crate::MessageId::QuestionAnswerLabel),
        &question_input_text(model, i18n, false),
        width,
    );
    let ghost_style = match model.input_feedback {
        QuestionInputFeedback::Required
        | QuestionInputFeedback::Invalid
        | QuestionInputFeedback::SelectionRequired => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::DIM),
        QuestionInputFeedback::Disabled | QuestionInputFeedback::None => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::DIM),
    };
    rows.into_iter()
        .map(|row| {
            if model.custom_answer.trim().is_empty() {
                let marker = "  › ";
                if let Some(index) = row.find(marker) {
                    return Line::from(vec![
                        Span::raw(row[..index].to_string()),
                        Span::styled(row[index..].to_string(), ghost_style),
                    ]);
                }
            }
            Line::from(row)
        })
        .collect()
}

fn wrap_option_text(prefix: &str, text: &str, width: usize) -> Vec<String> {
    let width = width.max(display_width(prefix) + 1);
    let continuation = " ".repeat(display_width(prefix));
    wrap_plain_line_with_prefix(text, prefix, &continuation, width)
}

fn option_is_checked(model: &QuestionPanelModel<'_>, idx: usize) -> bool {
    model.selection_mode == QuestionSelectionMode::Multiple && model.selected_options.contains(&idx)
}
