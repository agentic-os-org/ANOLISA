//! Rendering and lifecycle helpers for interactive auth prompts.

use std::io::{self, Write};

use crate::runtime::prelude::{
    I18n, MessageId, QuestionInputFeedback, QuestionPanelModel, QuestionSelectionMode,
    RatatuiInlineRenderer,
};
use crate::runtime::state::InlineState;

use super::capture::auth_capture_id;
use super::delete_confirm::render_delete_confirmation;
use super::menu::{existing_provider_label, management_options};
use super::provider_display::provider_option;
use super::provider_management::provider_action_options;
use super::runtime::AuthPhase;

pub(super) fn render_current_auth_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    let Some(auth) = &state.auth.state else {
        return Ok(());
    };
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let i18n = I18n::new(state.language);
    let panel_id = auth_capture_id(auth);

    match auth.phase {
        AuthPhase::ManagingProviders => {
            let options = management_options(&auth.sysom, &auth.existing_providers);

            let model = QuestionPanelModel {
                id: &panel_id,
                question: "\u{1f511} Provider Management \u{2014} Select your AI provider:",
                options: &options,
                selected_option: auth.selected_provider,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: false,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::Disabled,
            };
            let height = renderer.write_question_panel(output, model)?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id.clone());
        }
        AuthPhase::ProviderAction { provider_idx } => {
            let provider = &auth.existing_providers[provider_idx];
            let title = format!(
                "\u{1f511} {} \u{2014} \"{}\":",
                existing_provider_label(&auth.sysom, &auth.existing_providers, provider_idx),
                provider.name
            );
            let options = provider_action_options(
                provider.is_active,
                provider.editable,
                provider.deletable(),
            );
            let model = QuestionPanelModel {
                id: &panel_id,
                question: &title,
                options: &options,
                selected_option: auth.selected_provider,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: false,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::Disabled,
            };
            let height = renderer.write_question_panel(output, model)?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id.clone());
        }
        AuthPhase::ConfirmDelete { provider_idx } => {
            let height =
                render_delete_confirmation(auth, provider_idx, &panel_id, &renderer, output)?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id.clone());
        }
        AuthPhase::SelectingProvider => {
            let options: Vec<String> = auth
                .providers
                .iter()
                .map(|provider| provider_option(provider, state.language))
                .collect();
            let model = QuestionPanelModel {
                id: &panel_id,
                question: i18n.t(MessageId::AuthSelectProviderQuestion),
                options: &options,
                selected_option: auth.selected_provider,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: false,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::Disabled,
            };
            let height = renderer.write_question_panel(output, model)?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id.clone());
        }
        AuthPhase::FillingField => {
            let field = auth.current_field_info();
            let label = field.map(|field| field.label.as_str()).unwrap_or("Value");
            let is_secret = field.map(|field| field.secret).unwrap_or(false);
            let hint_text = field.and_then(|field| field.hint.as_deref()).unwrap_or("");
            let provider = auth.current_provider();
            let is_editing = auth.editing_provider_name.is_some();
            let action = if is_editing { "Edit" } else { "Enter" };
            let mut question = format!(
                "\u{1f511} {} \u{2014} {} {}:",
                provider.label, action, label
            );
            if !hint_text.is_empty() {
                question.push_str(&format!("\n  hint: {hint_text}"));
            }
            if let Some(error) = auth.field_error.as_deref() {
                question.push_str(&format!("\n  error: {error}"));
            }
            if is_editing && !auth.field_input.is_empty() {
                question.push_str("\n  (Enter to keep current value)");
            }
            if !auth.field_input.is_empty() {
                let display = if is_secret {
                    "\u{2022}".repeat(auth.field_input.len())
                } else {
                    auth.field_input.clone()
                };
                question.push_str(&format!("\n  > {display}"));
            } else {
                question.push_str("\n  > ");
            }
            let model = QuestionPanelModel {
                id: &panel_id,
                question: &question,
                options: &[],
                selected_option: 0,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: true,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::Disabled,
            };
            let height = renderer.write_question_panel(output, model)?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id.clone());
        }
        AuthPhase::PreparingMenu
        | AuthPhase::AliyunEcsPreparing
        | AuthPhase::AliyunEcsChallenge { .. } => {
            let (mut question, options, show_link) = super::ecs_poll::question(state);
            if show_link {
                if let AuthPhase::AliyunEcsChallenge {
                    instance_id,
                    console_url,
                } = &auth.phase
                {
                    question.push_str(&format!(
                        "\nECS Instance ID: {instance_id}\nURL: {console_url}"
                    ));
                    if let Some(qr) = generate_qr_text(console_url) {
                        question.push_str("\n\n");
                        question.push_str(&qr);
                    }
                }
            }
            let model = QuestionPanelModel {
                id: &panel_id,
                question: &question,
                options: &options,
                selected_option: 0,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: false,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::Disabled,
            };
            let height = renderer.write_question_panel(output, model)?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id);
        }
    }
    state.questions.active_panel_width = Some(renderer.panel_standard_width());
    output.flush()
}

pub(super) fn clear_active_auth_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    let height = state.questions.active_panel_height;
    if height == 0 {
        state.questions.active_panel_id = None;
        state.questions.active_panel_cursor_row = None;
        state.questions.active_panel_width = None;
        return Ok(());
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
    write!(output, "\r")?;
    state.questions.active_panel_id = None;
    state.questions.active_panel_height = 0;
    state.questions.active_panel_cursor_row = None;
    state.questions.active_panel_width = None;
    Ok(())
}

fn generate_qr_text(data: &str) -> Option<String> {
    use qrcode::QrCode;

    let code = QrCode::new(data.as_bytes()).ok()?;
    let width = code.width();
    let colors = code.to_colors();
    let margin = 2usize;
    let total_width = width + 2 * margin;
    let light_row: String = "\u{2588}".repeat(total_width);
    let mut result = String::new();

    for _ in 0..margin {
        result.push_str(&light_row);
        result.push('\n');
    }

    let mut y = 0;
    while y < width {
        for _ in 0..margin {
            result.push('\u{2588}');
        }
        for x in 0..width {
            let top_dark = colors[y * width + x] == qrcode::Color::Dark;
            let bottom_dark = if y + 1 < width {
                colors[(y + 1) * width + x] == qrcode::Color::Dark
            } else {
                false
            };
            result.push(match (top_dark, bottom_dark) {
                (true, true) => ' ',
                (true, false) => '\u{2584}',
                (false, true) => '\u{2580}',
                (false, false) => '\u{2588}',
            });
        }
        for _ in 0..margin {
            result.push('\u{2588}');
        }
        result.push('\n');
        y += 2;
    }

    for _ in 0..margin {
        result.push_str(&light_row);
        result.push('\n');
    }

    Some(result)
}
