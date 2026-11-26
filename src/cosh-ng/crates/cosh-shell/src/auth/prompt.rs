//! Rendering and lifecycle helpers for interactive auth prompts.

use std::io::{self, Write};

use crate::runtime::prelude::{
    I18n, MessageId, QuestionInputFeedback, QuestionPanelModel, QuestionPanelPresentation,
    QuestionSelectionMode, RatatuiInlineRenderer,
};
use crate::runtime::state::InlineState;

use super::capture::auth_capture_id;
use super::delete_confirm::render_delete_confirmation;
use super::host::AuthHost;
use super::menu::{existing_provider_label, hosted_options};
use super::provider_display::provider_option;
use super::provider_management::provider_action_options;
use super::runtime::AuthPhase;

pub(super) fn render_current_auth_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> io::Result<()> {
    let terminal_rows = ratatui::crossterm::terminal::size()
        .ok()
        .map(|(_, rows)| rows);
    render_current_auth_panel_with_rows(state, output, terminal_rows)
}

/// Renders the active auth card; `terminal_rows` may be reduced by rows a notice
/// already occupies above the card on the hosted guide screen.
pub(super) fn render_current_auth_panel_with_rows<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    terminal_rows: Option<u16>,
) -> io::Result<()> {
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    render_auth_panel(state, output, &renderer, terminal_rows)
}

pub(super) fn render_auth_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
    renderer: &RatatuiInlineRenderer,
    terminal_rows: Option<u16>,
) -> io::Result<()> {
    let Some(auth) = &state.auth.state else {
        return Ok(());
    };
    let i18n = I18n::new(state.language);
    let panel_id = auth_capture_id(auth);

    match auth.phase {
        AuthPhase::ManagingProviders => {
            let options = hosted_options(
                state.auth.host,
                &auth.sysom,
                &auth.existing_providers,
                state.i18n(),
            );

            let mut question = i18n
                .t(MessageId::AuthProviderManagementQuestion)
                .to_string();
            if !state.auth.blocked_writes.is_empty() {
                question = format!(
                    "{}\n{question}",
                    state
                        .i18n()
                        .t(crate::i18n::MessageId::GuideWizardAiWritesHeld)
                );
            }
            let model = QuestionPanelModel {
                id: &panel_id,
                question: &question,
                options: &options,
                selected_option: auth.selected_provider,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: false,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::Disabled,
            };
            let height = renderer.write_question_panel_with_presentation(
                output,
                model,
                auth_presentation(state.auth.host, i18n, &auth.phase, false, terminal_rows),
            )?;
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
            let height = renderer.write_question_panel_with_presentation(
                output,
                model,
                auth_presentation(state.auth.host, i18n, &auth.phase, false, terminal_rows),
            )?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id.clone());
        }
        AuthPhase::ConfirmDelete { provider_idx } => {
            let height = render_delete_confirmation(
                auth,
                provider_idx,
                &panel_id,
                renderer,
                auth_presentation(state.auth.host, i18n, &auth.phase, false, terminal_rows),
                output,
            )?;
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
                question: i18n.t(if state.auth.host.is_guide() {
                    MessageId::GuideAuthTemplateTitle
                } else {
                    MessageId::AuthSelectProviderQuestion
                }),
                options: &options,
                selected_option: auth.selected_provider,
                selected_options: &[],
                custom_answer: "",
                allow_free_text: false,
                selection_mode: QuestionSelectionMode::Single,
                input_feedback: QuestionInputFeedback::Disabled,
            };
            let height = renderer.write_question_panel_with_presentation(
                output,
                model,
                auth_presentation(state.auth.host, i18n, &auth.phase, false, terminal_rows),
            )?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id.clone());
        }
        AuthPhase::FillingField => {
            let field = auth.current_field_info();
            let mut label = field.map(|field| field.label.as_str()).unwrap_or("Value");
            let is_secret = field.map(|field| field.secret).unwrap_or(false);
            let mut hint_text = field.and_then(|field| field.hint.as_deref()).unwrap_or("");
            let provider = auth.current_provider();
            let is_editing = auth.editing_provider_name.is_some();
            let guided = state.auth.host.is_guide();
            if guided && provider.id == "openai_compat" {
                if let Some((name, hint)) = field.and_then(|field| match field.name.as_str() {
                    "base_url" => {
                        Some((MessageId::GuideAuthBaseUrl, MessageId::GuideAuthBaseUrlHint))
                    }
                    "api_key" => Some((MessageId::GuideAuthApiKey, MessageId::GuideAuthApiKeyHint)),
                    "model" => Some((MessageId::GuideAuthModel, MessageId::GuideAuthModelHint)),
                    _ => None,
                }) {
                    label = i18n.t(name);
                    hint_text = i18n.t(hint);
                }
            }
            let action = if is_editing { "Edit" } else { "Enter" };
            let mut question = if guided {
                i18n.format(
                    if is_editing {
                        MessageId::GuideAuthEditField
                    } else {
                        MessageId::GuideAuthEnterField
                    },
                    &[("service", &provider.label), ("field", label)],
                )
            } else {
                format!(
                    "\u{1f511} {} \u{2014} {} {}:",
                    provider.label, action, label
                )
            };
            if !guided && !hint_text.is_empty() {
                question.push_str(&format!("\n  hint: {hint_text}"));
            }
            if let Some(error) = auth.field_error.as_deref() {
                question.push('\n');
                question.push_str(&if guided {
                    i18n.format(MessageId::GuideAuthFieldError, &[("value", error)])
                } else {
                    format!("  error: {error}")
                });
            }
            if guided && !hint_text.is_empty() {
                question.push('\n');
                question
                    .push_str(&i18n.format(MessageId::GuideAuthFieldHint, &[("value", hint_text)]));
            }
            if is_editing && !auth.field_input.is_empty() {
                question.push('\n');
                question.push_str(if guided {
                    i18n.t(MessageId::GuideAuthKeepField)
                } else {
                    "  (Enter to keep current value)"
                });
            }
            let details_end = question.len();
            let last_guide_field = guided
                && auth.editable_field_at_or_after(auth.current_field + 1) >= provider.fields.len();
            if last_guide_field {
                for line in guide_validation_body(
                    i18n,
                    &provider.id,
                    auth.collected_values.get("auth_source").map(String::as_str),
                ) {
                    question.push('\n');
                    question.push_str(line);
                }
            }
            if !auth.field_input.is_empty() {
                let display = if is_secret {
                    "\u{2022}".repeat(auth.field_input.len())
                } else {
                    auth.field_input.clone()
                };
                if guided
                    && crate::ui::display_width(&display)
                        > renderer.question_body_width().saturating_sub(4)
                {
                    let limit = renderer.question_body_width().saturating_sub(5);
                    let mut width = 0;
                    let suffix = display
                        .chars()
                        .rev()
                        .take_while(|ch| {
                            width += crate::ui::display_width(ch.encode_utf8(&mut [0; 4]));
                            width <= limit
                        })
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<String>();
                    question.push_str(&format!("\n  > …{suffix}"));
                } else {
                    question.push_str(&format!("\n  > {display}"));
                }
            } else {
                question.push_str("\n  > ");
            }
            let mut model = QuestionPanelModel {
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
            let mut presentation =
                auth_presentation(state.auth.host, i18n, &auth.phase, false, terminal_rows);
            let bounded_question;
            if last_guide_field {
                let mut bounded = presentation;
                bounded.max_rows = terminal_rows;
                if let Some(body) =
                    renderer.question_with_required_tail(&model, bounded, details_end)
                {
                    bounded_question = body;
                    model.question = &bounded_question;
                    presentation = bounded;
                }
            }
            let height =
                renderer.write_question_panel_with_presentation(output, model, presentation)?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id.clone());
        }
        AuthPhase::PreparingMenu
        | AuthPhase::AliyunEcsPreparing
        | AuthPhase::AliyunEcsChallenge { .. } => {
            let (mut question, options, show_link) = super::ecs_poll::question(state);
            let presentation = auth_presentation(
                state.auth.host,
                i18n,
                &auth.phase,
                options.is_empty(),
                terminal_rows,
            );
            if show_link {
                if let AuthPhase::AliyunEcsChallenge {
                    instance_id,
                    console_url,
                } = &auth.phase
                {
                    let qr = generate_qr_text(console_url);
                    if state.auth.host.is_guide() {
                        let probe = QuestionPanelModel {
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
                        let budget = renderer.question_body_budget(&probe, i18n, presentation);
                        question = ecs_challenge_body(
                            &question,
                            instance_id,
                            console_url,
                            qr.as_deref(),
                            budget,
                            renderer.question_body_width(),
                        );
                    } else {
                        question.push_str(&format!(
                            "\nECS Instance ID: {instance_id}\nURL: {console_url}"
                        ));
                        if let Some(qr) = qr {
                            question.push_str("\n\n");
                            question.push_str(&qr);
                        }
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
            let height =
                renderer.write_question_panel_with_presentation(output, model, presentation)?;
            state.questions.active_panel_height = height;
            state.questions.active_panel_id = Some(panel_id);
        }
    }
    state.questions.active_panel_width = Some(renderer.panel_standard_width());
    output.flush()
}

pub(super) fn guide_validation_body(
    i18n: I18n,
    provider_id: &str,
    auth_source: Option<&str>,
) -> Vec<&'static str> {
    let mut lines = vec![i18n.t(MessageId::GuideAuthValidationChecks)];
    if may_validate_with_chat(provider_id, auth_source) {
        lines.push(i18n.t(MessageId::GuideAuthValidationUsage));
    }
    lines
}

pub(super) fn auth_validation_body(provider_id: &str, auth_source: Option<&str>) -> Vec<String> {
    let mut body = vec!["Checking endpoint, credentials, and model.".to_string()];
    if may_validate_with_chat(provider_id, auth_source) {
        body.push(
            "If read-only model validation is unavailable or cannot verify credentials, validation may make a minimal chat request (max_tokens=1) that consumes a very small amount of quota or incurs a very small charge."
                .to_string(),
        );
    }
    body
}

fn may_validate_with_chat(provider_id: &str, auth_source: Option<&str>) -> bool {
    matches!(
        provider_id,
        "dashscope" | "openai_compat" | "coding_plan" | "token_plan"
    ) || (provider_id == "aliyun" && auth_source != Some("ecs_ram_role"))
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

pub(super) fn generate_qr_text(data: &str) -> Option<String> {
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

/// Assembles the ECS challenge body; the guide budgets rows so actions and the
/// full URL survive, while an unbounded panel keeps the legacy body with the QR.
pub(super) fn ecs_challenge_body(
    base: &str,
    instance_id: &str,
    console_url: &str,
    qr: Option<&str>,
    budget: Option<usize>,
    width: usize,
) -> String {
    let id_line = format!("ECS Instance ID: {instance_id}");
    let url_line = format!("URL: {console_url}");
    let with_url = format!("{base}\n{id_line}\n{url_line}");
    let mut full = with_url.clone();
    if let Some(qr) = qr {
        full.push_str("\n\n");
        full.push_str(qr);
    }
    let Some(budget) = budget else {
        return full;
    };
    // Degrade the QR whole first, then the instance id, so the copyable URL and
    // the actionable status hint outlive the decoration on a short screen.
    let rows = |text: &str| crate::ui::agent_render::wrap_plain_line(text, width).len();
    if rows(&full) <= budget {
        return full;
    }
    if rows(&with_url) <= budget {
        return with_url;
    }
    let url_only = format!("{base}\n{url_line}");
    if rows(&url_only) <= budget {
        return url_only;
    }
    base.to_string()
}

/// The guide runs auth as a configuration task, so its footers name navigation
/// instead of sending an answer; the Shell host keeps the default question copy.
pub(super) fn auth_presentation(
    host: AuthHost,
    i18n: I18n,
    phase: &AuthPhase,
    status_options_empty: bool,
    terminal_rows: Option<u16>,
) -> QuestionPanelPresentation<'static> {
    if !host.is_guide() {
        return QuestionPanelPresentation::default();
    }
    let keys = match phase {
        AuthPhase::ManagingProviders
        | AuthPhase::ProviderAction { .. }
        | AuthPhase::SelectingProvider => MessageId::GuideAiManagementKeys,
        AuthPhase::ConfirmDelete { .. } => MessageId::GuideAuthDeleteKeys,
        AuthPhase::FillingField => {
            // The typed value is echoed on the last question line and viewport
            // windowing only scrolls options, so a row cap could hide the input;
            // question_cursor_placement is also viewport-unaware. Keep the
            // free-text panel unbounded and only correct its key copy.
            return QuestionPanelPresentation {
                instruction: Some(i18n.t(MessageId::GuideAuthFieldKeys)),
                ..Default::default()
            };
        }
        AuthPhase::PreparingMenu
        | AuthPhase::AliyunEcsPreparing
        | AuthPhase::AliyunEcsChallenge { .. } => {
            if status_options_empty {
                // A waiting status page has no selectable choice; its body already
                // carries the cancel hint, so only the row cap applies.
                return QuestionPanelPresentation {
                    max_rows: terminal_rows,
                    ..Default::default()
                };
            }
            MessageId::GuideAiManagementKeys
        }
    };
    QuestionPanelPresentation {
        instruction: Some(i18n.t(keys)),
        max_rows: terminal_rows,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Language;

    #[test]
    fn guide_subpage_presentations_name_navigation_and_keep_shell_defaults() {
        for language in [Language::ZhCn, Language::EnUs] {
            let i18n = I18n::new(language);
            let guide = auth_presentation(
                AuthHost::Guide,
                i18n,
                &AuthPhase::ManagingProviders,
                false,
                Some(16),
            );
            let instruction = guide.instruction.expect("guide instruction");
            assert!(instruction.contains("Esc"), "{instruction}");
            assert!(instruction.contains("Ctrl-D"), "{instruction}");
            assert!(!instruction.contains("发送"), "{instruction}");
            assert!(!instruction.contains("send"), "{instruction}");
            assert_eq!(guide.title, None);
            assert_eq!(guide.max_rows, Some(16));
            let shell = auth_presentation(
                AuthHost::Shell,
                i18n,
                &AuthPhase::ManagingProviders,
                false,
                Some(16),
            );
            assert!(shell.instruction.is_none());
            assert_eq!(shell.max_rows, None);
            assert_eq!(
                auth_presentation(
                    AuthHost::Guide,
                    i18n,
                    &AuthPhase::ManagingProviders,
                    false,
                    None
                )
                .max_rows,
                None
            );

            for phase in [
                AuthPhase::ProviderAction { provider_idx: 0 },
                AuthPhase::SelectingProvider,
            ] {
                let list = auth_presentation(AuthHost::Guide, i18n, &phase, false, Some(16));
                let keys = list.instruction.expect("selection keys");
                assert!(keys.contains("Enter"), "{keys}");
                assert!(keys.contains("Esc"), "{keys}");
                assert!(!keys.contains("发送"), "{keys}");
                assert!(!keys.contains("send"), "{keys}");
                assert_eq!(list.max_rows, Some(16));
                let shell_list = auth_presentation(AuthHost::Shell, i18n, &phase, false, Some(16));
                assert_eq!(shell_list, QuestionPanelPresentation::default());
            }

            let delete = auth_presentation(
                AuthHost::Guide,
                i18n,
                &AuthPhase::ConfirmDelete { provider_idx: 0 },
                false,
                Some(16),
            );
            let delete_keys = delete.instruction.expect("delete keys");
            assert!(delete_keys.contains("Esc"), "{delete_keys}");
            assert!(delete_keys.contains("Ctrl-D"), "{delete_keys}");
            match language {
                Language::ZhCn => assert!(delete_keys.contains("确认"), "{delete_keys}"),
                Language::EnUs => assert!(delete_keys.contains("confirm"), "{delete_keys}"),
            }
            assert_eq!(delete.max_rows, Some(16));
            assert_eq!(
                auth_presentation(
                    AuthHost::Shell,
                    i18n,
                    &AuthPhase::ConfirmDelete { provider_idx: 0 },
                    false,
                    Some(16)
                ),
                QuestionPanelPresentation::default()
            );

            let field = auth_presentation(
                AuthHost::Guide,
                i18n,
                &AuthPhase::FillingField,
                false,
                Some(16),
            );
            let field_keys = field.instruction.expect("field keys");
            assert!(field_keys.contains("Esc"), "{field_keys}");
            assert!(!field_keys.contains("发送"), "{field_keys}");
            assert!(!field_keys.contains("send"), "{field_keys}");
            match language {
                Language::ZhCn => assert!(field_keys.contains("输入"), "{field_keys}"),
                Language::EnUs => assert!(field_keys.contains("Type"), "{field_keys}"),
            }
            // Free-text echo lives at the end of the question block; bounding it
            // could hide the input line, so the field panel stays unbounded.
            assert_eq!(field.max_rows, None);
            assert_eq!(
                auth_presentation(
                    AuthHost::Shell,
                    i18n,
                    &AuthPhase::FillingField,
                    false,
                    Some(16)
                ),
                QuestionPanelPresentation::default()
            );

            for phase in [
                AuthPhase::PreparingMenu,
                AuthPhase::AliyunEcsPreparing,
                AuthPhase::AliyunEcsChallenge {
                    instance_id: "i-test".to_string(),
                    console_url: "https://example.invalid".to_string(),
                },
            ] {
                let status = auth_presentation(AuthHost::Guide, i18n, &phase, true, Some(16));
                assert_eq!(status.instruction, None);
                assert_eq!(status.max_rows, Some(16));
                let actionable = auth_presentation(AuthHost::Guide, i18n, &phase, false, Some(16));
                let keys = actionable.instruction.expect("status action keys");
                assert!(keys.contains("Esc"), "{keys}");
                assert_eq!(actionable.max_rows, Some(16));
                assert_eq!(
                    auth_presentation(AuthHost::Shell, i18n, &phase, true, Some(16)),
                    QuestionPanelPresentation::default()
                );
            }
        }
    }
}
