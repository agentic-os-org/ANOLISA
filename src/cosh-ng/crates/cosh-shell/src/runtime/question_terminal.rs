use std::io::Write;

#[cfg(test)]
use crate::question::runtime::RuntimeUserQuestion;
use crate::runtime::state::InlineState;
use crate::ui::{QuestionInputFeedback, QuestionPanelModel, RatatuiInlineRenderer};

pub(crate) fn clear_active_question_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let height = state.questions.active_panel_height;
    if height == 0 {
        state.questions.active_panel_id = None;
        state.questions.active_panel_cursor_row = None;
        state.questions.active_panel_width = None;
        return Ok(());
    }

    if let Some(cursor_row) = state.questions.active_panel_cursor_row.take() {
        let down = height.saturating_sub(cursor_row);
        if down > 0 {
            write!(output, "\x1b[{down}B")?;
        }
        write!(output, "\r")?;
    }
    write!(output, "\x1b[{height}A")?;
    for row in 0..height {
        write!(output, "\r\x1b[2K")?;
        if row + 1 < height {
            write!(output, "\x1b[1B")?;
        }
    }
    if height > 1 {
        write!(output, "\x1b[{}A", height - 1)?;
    }
    write!(output, "\r\x1b[?25h")?;
    state.questions.active_panel_id = None;
    state.questions.active_panel_height = 0;
    state.questions.active_panel_cursor_row = None;
    state.questions.active_panel_width = None;
    Ok(())
}

pub(crate) fn cleanup_question_for_terminal_owner<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    owner_request_id: &str,
) -> std::io::Result<bool> {
    let Some(question_id) = state.questions.pending_id.clone() else {
        return Ok(false);
    };
    let Some(question_index) = state.questions.items.iter().position(|question| {
        question.id == question_id
            && question.answer.is_none()
            && question.provider_request_id.is_some()
            && question.provider_owner_request_id.as_deref() == Some(owner_request_id)
    }) else {
        return Ok(false);
    };

    clear_active_question_panel(state, output)?;
    state.questions.items[question_index].input_feedback = QuestionInputFeedback::None;
    state.questions.pending_id = None;
    state.questions.active_panel_id = None;
    state.questions.active_panel_height = 0;
    state.questions.active_panel_cursor_row = None;
    state.questions.active_panel_width = None;
    state.questions.question_protocol_failure_reported = false;
    Ok(true)
}

pub(crate) fn redraw_active_question_if_width_changed<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    renderer: RatatuiInlineRenderer,
) -> std::io::Result<bool> {
    let width = renderer.panel_standard_width();
    if state.questions.active_panel_width == Some(width) {
        return Ok(false);
    }
    let Some(active_id) = state.questions.active_panel_id.clone() else {
        return Ok(false);
    };
    let Some(question) = state
        .questions
        .items
        .iter()
        .find(|question| question.id == active_id && question.answer.is_none())
        .cloned()
    else {
        return Ok(false);
    };
    let model = QuestionPanelModel {
        id: &question.id,
        question: &question.question,
        options: &question.options,
        selected_option: question.selected_option,
        selected_options: &question.selected_options,
        custom_answer: &question.custom_answer,
        allow_free_text: question.allow_free_text,
        selection_mode: question.selection_mode,
        input_feedback: question.input_feedback,
    };
    clear_active_question_panel(state, output)?;
    let cursor_row = renderer
        .active_question_cursor_placement(&model)
        .map(|placement| placement.row);
    let height = renderer.write_question_panel(output, model)?;
    state.questions.active_panel_id = Some(question.id);
    state.questions.active_panel_height = height;
    state.questions.active_panel_cursor_row = cursor_row;
    state.questions.active_panel_width = Some(width);
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::prelude::AgentRunOrigin;
    use crate::types::QuestionSelectionMode;

    fn pending_state() -> InlineState {
        let mut state = InlineState::default();
        state.questions.items.push(RuntimeUserQuestion {
            id: "q-1".to_string(),
            question: "Choose".to_string(),
            options: Vec::new(),
            selected_option: 0,
            selected_options: Vec::new(),
            custom_answer: String::new(),
            allow_free_text: true,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::Required,
            provider_request_id: Some("provider-q".to_string()),
            provider_owner_request_id: Some("owner".to_string()),
            origin: AgentRunOrigin::Standard,
            answer: None,
        });
        state.questions.pending_id = Some("q-1".to_string());
        state.questions.active_panel_id = Some("q-1".to_string());
        state.questions.active_panel_height = 4;
        state.questions.active_panel_cursor_row = Some(2);
        state
    }

    #[test]
    fn terminal_cleanup_restores_anchor_and_only_clears_matching_owner() {
        let mut state = pending_state();
        let mut output = Vec::new();
        assert!(
            !cleanup_question_for_terminal_owner(&mut state, &mut output, "other")
                .expect("owner mismatch")
        );
        assert_eq!(state.questions.pending_id.as_deref(), Some("q-1"));
        assert!(output.is_empty());

        assert!(
            cleanup_question_for_terminal_owner(&mut state, &mut output, "owner")
                .expect("owner cleanup")
        );
        assert!(state.questions.pending_id.is_none());
        assert!(state.questions.items[0].answer.is_none());
        assert!(state.questions.active_panel_cursor_row.is_none());
        let text = String::from_utf8(output).expect("terminal bytes");
        assert!(text.starts_with("\u{1b}[2B\r\u{1b}[4A"), "{text:?}");
        assert!(text.ends_with("\u{1b}[?25h"), "{text:?}");
    }

    #[test]
    fn terminal_cleanup_keeps_non_provider_question() {
        let mut state = pending_state();
        state.questions.items[0].provider_request_id = None;
        let mut output = Vec::new();

        assert!(
            !cleanup_question_for_terminal_owner(&mut state, &mut output, "owner")
                .expect("non-provider question")
        );
        assert_eq!(state.questions.pending_id.as_deref(), Some("q-1"));
        assert_eq!(state.questions.active_panel_id.as_deref(), Some("q-1"));
        assert!(output.is_empty());
    }
}
