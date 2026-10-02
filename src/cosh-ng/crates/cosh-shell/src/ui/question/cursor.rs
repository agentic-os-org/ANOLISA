use std::io::IsTerminal;

use crate::question::choices::question_custom_answer_index;
use crate::ui::agent_render::{display_width, RatatuiInlineRenderer};

use super::{
    custom_option_label, option_line_text, question_content_width, question_input_text,
    question_rows, selected_option, wrap_option_text, wrapped_question_label_rows,
    wrapped_row_count, QuestionPanelModel,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuestionCursorPlacement {
    pub row: usize,
    pub column: usize,
}

impl RatatuiInlineRenderer {
    pub fn question_cursor_placement(
        &self,
        model: &QuestionPanelModel<'_>,
    ) -> Option<QuestionCursorPlacement> {
        if !model.allow_free_text || model.input_feedback == super::QuestionInputFeedback::Disabled
        {
            return None;
        }
        if self.plain {
            return plain_question_cursor_placement(self, model);
        }
        let content_width = question_content_width(self.panel_standard_width());
        let input = question_input_text(model, self.i18n(), false);
        if model.options.is_empty() {
            let rows = wrapped_question_label_rows(
                self.i18n().t(crate::MessageId::QuestionAnswerLabel),
                &input,
                content_width,
            );
            let row = 1 + question_rows(model, content_width) as usize + rows.len() - 1;
            let prefix_width =
                display_width(self.i18n().t(crate::MessageId::QuestionAnswerLabel)) + 2;
            let input_width = if model.custom_answer.trim().is_empty() {
                0
            } else {
                display_width(rows.last().map(String::as_str).unwrap_or_default())
                    .saturating_sub(if rows.len() == 1 { prefix_width } else { 0 })
            };
            return Some(QuestionCursorPlacement {
                row,
                column: 2 + if rows.len() == 1 { prefix_width } else { 0 } + input_width,
            });
        }

        let custom_index =
            question_custom_answer_index(model.options.len(), model.allow_free_text)?;
        if selected_option(model) != custom_index {
            return None;
        }
        let preceding_rows = model
            .options
            .iter()
            .enumerate()
            .map(|(idx, option)| {
                wrapped_row_count(&option_line_text(model, idx, option), content_width)
            })
            .sum::<u16>();
        let prefix = format!("> [{}] ", custom_index + 1);
        let label = custom_option_label(self.i18n(), model.custom_answer);
        let custom_rows = wrap_option_text(&prefix, &label, content_width);
        Some(QuestionCursorPlacement {
            row: 1
                + question_rows(model, content_width) as usize
                + 1
                + preceding_rows as usize
                + custom_rows.len()
                - 1,
            column: 2 + if model.custom_answer.trim().is_empty() {
                display_width(&format!(
                    "{prefix}{}  ",
                    self.i18n().t(crate::MessageId::QuestionOtherEmptyLabel)
                ))
            } else {
                display_width(custom_rows.last().map(String::as_str).unwrap_or_default())
            },
        })
    }

    pub(super) fn supports_question_cursor(&self) -> bool {
        (self.styled || (std::env::var_os("NO_COLOR").is_some() && std::io::stdout().is_terminal()))
            && std::env::var("TERM").ok().as_deref() != Some("dumb")
    }

    pub(crate) fn active_question_cursor_placement(
        &self,
        model: &QuestionPanelModel<'_>,
    ) -> Option<QuestionCursorPlacement> {
        self.supports_question_cursor()
            .then(|| self.question_cursor_placement(model))
            .flatten()
    }
}

fn plain_question_cursor_placement(
    renderer: &RatatuiInlineRenderer,
    model: &QuestionPanelModel<'_>,
) -> Option<QuestionCursorPlacement> {
    let content_width = question_content_width(renderer.panel_standard_width());
    if model.options.is_empty() {
        let rows = wrapped_question_label_rows(
            renderer.i18n().t(crate::MessageId::QuestionAnswerLabel),
            &question_input_text(model, renderer.i18n(), true),
            content_width,
        );
        let prefix_width =
            display_width(renderer.i18n().t(crate::MessageId::QuestionAnswerLabel)) + 2;
        let input_width = if model.custom_answer.trim().is_empty() {
            0
        } else {
            display_width(rows.last().map(String::as_str).unwrap_or_default())
                .saturating_sub(if rows.len() == 1 { prefix_width } else { 0 })
        };
        return Some(QuestionCursorPlacement {
            row: 1 + question_rows(model, content_width) as usize + rows.len() - 1,
            column: if rows.len() == 1 { prefix_width } else { 0 } + input_width,
        });
    }

    let custom_index = question_custom_answer_index(model.options.len(), model.allow_free_text)?;
    if selected_option(model) != custom_index {
        return None;
    }
    let preceding_rows = model
        .options
        .iter()
        .enumerate()
        .map(|(idx, option)| {
            wrapped_row_count(&option_line_text(model, idx, option), content_width)
        })
        .sum::<u16>();
    let prefix = format!("> [{}] ", custom_index + 1);
    let label = custom_option_label(renderer.i18n(), model.custom_answer);
    let rows = wrap_option_text(&prefix, &label, content_width);
    Some(QuestionCursorPlacement {
        row: 1
            + question_rows(model, content_width) as usize
            + 1
            + preceding_rows as usize
            + rows.len()
            - 1,
        column: if model.custom_answer.trim().is_empty() {
            display_width(&format!("{prefix}{label}  "))
        } else {
            display_width(rows.last().map(String::as_str).unwrap_or_default())
        },
    })
}
