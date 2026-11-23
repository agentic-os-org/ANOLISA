//! Optional labels for callers that present a specialized question workflow.

use super::{question_custom_answer_index, selected_option, wrapped_row_count, QuestionPanelModel};

/// Overrides question-card chrome without changing question input semantics.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QuestionPanelPresentation<'a> {
    pub(crate) title: Option<&'a str>,
    pub(crate) keys_prefix: Option<&'a str>,
    pub(crate) instruction: Option<&'a str>,
    /// Terminal rows available to the panel. The content budget reserves the last
    /// row for the trailing newline, so a panel written from the top of the screen
    /// never scrolls its own title away.
    pub(crate) max_rows: Option<u16>,
}

impl QuestionPanelPresentation<'_> {
    fn has_text_override(&self) -> bool {
        self.title.is_some() || self.keys_prefix.is_some() || self.instruction.is_some()
    }
}

impl<'a> QuestionPanelPresentation<'a> {
    /// Builds a complete title and keyboard-instruction override.
    pub(crate) const fn new(title: &'a str, keys_prefix: &'a str, instruction: &'a str) -> Self {
        Self {
            title: Some(title),
            keys_prefix: Some(keys_prefix),
            instruction: Some(instruction),
            max_rows: None,
        }
    }
}

/// Content rows a bounded panel may use; the floor keeps title, one content row
/// and the keys line renderable on pathological terminals.
pub(super) fn row_budget(max_rows: Option<u16>) -> Option<u16> {
    max_rows.map(|max| max.saturating_sub(1).max(3))
}

pub(super) fn instruction_rows(
    model: &QuestionPanelModel<'_>,
    i18n: crate::I18n,
    width: usize,
    presentation: QuestionPanelPresentation<'_>,
) -> u16 {
    if !presentation.has_text_override() {
        return wrapped_row_count(
            &default_instruction(model, i18n, selected_option(model)),
            width,
        );
    }
    wrapped_row_count(
        &instruction_text(model, i18n, selected_option(model), presentation, true),
        width,
    )
}

pub(super) fn instruction_text(
    model: &QuestionPanelModel<'_>,
    i18n: crate::I18n,
    selected_option: usize,
    presentation: QuestionPanelPresentation<'_>,
    include_prefix: bool,
) -> String {
    let instruction = presentation
        .instruction
        .map(str::to_owned)
        .unwrap_or_else(|| default_instruction(model, i18n, selected_option));
    if include_prefix && presentation.has_text_override() {
        format!(
            "{}{}",
            presentation
                .keys_prefix
                .unwrap_or_else(|| i18n.t(crate::MessageId::QuestionKeysPrefix)),
            instruction
        )
    } else {
        instruction
    }
}

fn default_instruction(
    model: &QuestionPanelModel<'_>,
    i18n: crate::I18n,
    selected_option: usize,
) -> String {
    if !model.options.is_empty() {
        let custom_selected =
            question_custom_answer_index(model.options.len(), model.allow_free_text)
                .is_some_and(|idx| selected_option >= idx);
        if model.selection_mode == crate::types::QuestionSelectionMode::Multiple {
            if custom_selected {
                i18n.t(crate::MessageId::QuestionInstructionMoveTypeSend)
                    .to_string()
            } else {
                i18n.t(crate::MessageId::QuestionInstructionMoveToggleSend)
                    .to_string()
            }
        } else if custom_selected {
            i18n.t(crate::MessageId::QuestionInstructionMoveTypeSend)
                .to_string()
        } else {
            i18n.t(crate::MessageId::QuestionInstructionMoveSend)
                .to_string()
        }
    } else if model.allow_free_text {
        i18n.t(crate::MessageId::QuestionInstructionTypeSend)
            .to_string()
    } else {
        i18n.t(crate::MessageId::QuestionInstructionNoAnswer)
            .to_string()
    }
}
