//! Focused-choice windowing for a height-bounded question panel.

use crate::question::choices::question_custom_answer_index;
use crate::ui::agent_render::RatatuiInlineRenderer;

use super::styles::render_custom_option_lines;
use super::{plain_option_lines, QuestionPanelModel};

/// Line offset and length of the focused choice inside the rendered option block.
pub(super) fn selected_option_window(
    model: &QuestionPanelModel<'_>,
    i18n: crate::I18n,
    selected: usize,
    width: usize,
) -> (usize, usize) {
    // The window indexes rendered lines, so rows must be counted with the renderer's full
    // prefix hanging indent; the plain-text estimate under-counts wrapped choices.
    let mut start = 1;
    for (idx, option) in model.options.iter().enumerate() {
        let rows = plain_option_lines(model, idx, option, width).len();
        if idx == selected {
            return (start, rows);
        }
        start += rows;
    }
    if let Some(idx) = question_custom_answer_index(model.options.len(), model.allow_free_text) {
        if idx == selected {
            return (
                start,
                render_custom_option_lines(
                    idx,
                    true,
                    i18n,
                    model.custom_answer,
                    model.input_feedback,
                    width,
                )
                .len(),
            );
        }
    }
    (1, 1)
}

/// Applies the row cap to the plain panel: title and question stay at the top,
/// the keys line stays at the bottom, and the choice block windows around the
/// focused option exactly like the bordered renderer, which reserves the focused
/// choice before the question so a tall body cannot drop the actions.
pub(super) fn bound_plain_lines(
    lines: Vec<String>,
    model: &QuestionPanelModel<'_>,
    i18n: crate::I18n,
    presentation: super::QuestionPanelPresentation<'_>,
    width: usize,
) -> Vec<String> {
    let Some(budget) = super::row_budget(presentation.max_rows) else {
        return lines;
    };
    let budget = budget as usize;
    if lines.len() <= budget {
        return lines;
    }
    let instruction_rows = super::wrap_plain_line(
        &super::presented_instruction_text(
            model,
            i18n,
            super::selected_option(model),
            presentation,
            true,
        ),
        width,
    )
    .len();
    let footer_start = lines.len().saturating_sub(instruction_rows);
    let question_rows = super::wrap_plain_line(model.question, width).len();
    let block_start = 1 + question_rows;
    let available = budget.saturating_sub(1 + instruction_rows);
    let (start, len) = if model.options.is_empty() {
        (1, 0)
    } else {
        selected_option_window(model, i18n, super::selected_option(model), width)
    };
    let question_keep = question_rows.min(available.saturating_sub(len.min(available)));
    let window = available.saturating_sub(question_keep);
    if model.options.is_empty() || window == 0 {
        let front = (1 + question_keep)
            .min(footer_start)
            .min(budget.saturating_sub(instruction_rows));
        let mut kept = lines[..front].to_vec();
        kept.extend(lines[footer_start..].iter().cloned());
        return kept;
    }
    let block = &lines[block_start..footer_start];
    let scroll = start
        .saturating_sub(window.saturating_sub(len).max(1) / 2)
        .min(block.len().saturating_sub(window));
    let mut kept = lines[..1 + question_keep].to_vec();
    kept.extend(block[scroll..scroll + window].iter().cloned());
    kept.extend(lines[footer_start..].iter().cloned());
    kept
}

impl RatatuiInlineRenderer {
    /// Content width of the question block inside the panel chrome.
    pub(crate) fn question_body_width(&self) -> usize {
        super::question_content_width(self.panel_standard_width())
    }

    /// Rows a bounded panel leaves for the question block once the keys line and
    /// the focused choice are reserved; content assembly must fit this budget.
    pub(crate) fn question_body_budget(
        &self,
        model: &QuestionPanelModel<'_>,
        i18n: crate::I18n,
        presentation: super::QuestionPanelPresentation<'_>,
    ) -> Option<usize> {
        let budget = super::row_budget(presentation.max_rows)?;
        let width = self.question_body_width();
        let instruction_rows = super::presented_instruction_rows(model, i18n, width, presentation);
        let chrome_rows = if self.plain { 1 } else { 2 };
        let available = budget.saturating_sub(chrome_rows + instruction_rows);
        let focus_rows = if model.options.is_empty() {
            0
        } else {
            selected_option_window(model, i18n, super::selected_option(model), width).1 as u16
        };
        Some(available.saturating_sub(focus_rows.min(available)) as usize)
    }

    pub(crate) fn question_with_required_tail(
        &self,
        model: &QuestionPanelModel<'_>,
        presentation: super::QuestionPanelPresentation<'_>,
        tail_start: usize,
    ) -> Option<String> {
        let width = self.question_body_width();
        let (details, required) = model.question.split_at(tail_start);
        let required = super::wrap_plain_line(required.trim_start_matches('\n'), width);
        let detail_rows = self
            .question_body_budget(model, self.i18n(), presentation)?
            .checked_sub(required.len())?;
        let mut rows = super::wrap_plain_line(details, width);
        rows.truncate(detail_rows);
        rows.extend(required);
        Some(rows.join("\n"))
    }
}
