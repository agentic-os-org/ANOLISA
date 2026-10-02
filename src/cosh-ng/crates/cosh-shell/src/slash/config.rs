use crate::runtime::prelude::*;
use crate::runtime::state::RuntimeConfigPanel;
use crate::slash::notices::render_info;
use crate::slash::panel::render_notice_panel;
use crate::slash::parser::SlashInfoCommand;
use crate::slash::prompt::write_shell_prompt;

pub(crate) fn render_config_command<W: Write>(
    sub: Option<&str>,
    value: Option<&str>,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    match (sub, value) {
        (None, None) => {
            render_info(SlashInfoCommand::Config, state, output)?;
            Ok(true)
        }
        (None, Some(_)) => {
            render_info(SlashInfoCommand::Config, state, output)?;
            Ok(true)
        }
        (Some("language"), Some(value)) => {
            let Some(setting) = parse_language_setting(value) else {
                let i18n = state.i18n();
                render_notice_panel(
                    output,
                    i18n.t(MessageId::SlashInfoConfigTitle),
                    vec![i18n.format(MessageId::ConfigInvalidLanguageBody, &[("language", value)])],
                    Some(i18n.t(MessageId::ConfigSupportedLanguagesFooter)),
                )?;
                return Ok(true);
            };
            let status = language_config_status();
            if !begin_config_language_confirmation(state, &status, setting.as_config_value()) {
                render_config_home_missing(state, output)?;
                return Ok(true);
            }
            render_current_config_panel(state, output)?;
            Ok(false)
        }
        (Some("language"), None) => {
            let status = language_config_status();
            state
                .control
                .set_pending_config_language_panel(language_option_index(&status.setting));
            render_current_config_language_panel(state, output)?;
            Ok(false)
        }
        (Some(other), _) => {
            let i18n = state.i18n();
            render_notice_panel(
                output,
                i18n.t(MessageId::SlashInfoConfigTitle),
                vec![i18n.format(MessageId::ConfigUnknownKeyBody, &[("key", other)])],
                Some(i18n.t(MessageId::ModeLanguageFooter)),
            )?;
            Ok(true)
        }
    }
}

fn begin_config_language_confirmation(
    state: &mut InlineState,
    status: &LanguageConfigStatus,
    pending_value: &str,
) -> bool {
    let Some(config_path) = status.config_path.clone() else {
        return false;
    };
    let panel = RuntimeConfigPanel {
        id: state.control.new_config_panel_id(),
        setting: "language".to_string(),
        before_value: status.setting.clone(),
        pending_value: pending_value.to_string(),
        config_path,
        selected_option: 0,
    };
    state.control.set_pending_config_panel(panel);
    true
}

fn render_config_home_missing<W: Write>(
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let i18n = state.i18n();
    render_notice_panel(
        output,
        i18n.t(MessageId::SlashInfoConfigTitle),
        vec![i18n.t(MessageId::ConfigHomeMissingBody).to_string()],
        Some(i18n.t(MessageId::ConfigHomeMissingFooter)),
    )
}

pub(crate) fn render_config_card_actions<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        if let Some(action) = config_language_card_action_from_event(event) {
            let key = format!(
                "{}:{}",
                stable_event_key("config-language-card", event_index, event),
                event.message.as_deref().unwrap_or_default()
            );
            if !state.control.claim_config_action(key) {
                continue;
            }
            match action {
                ConfigLanguageCardAction::Focus { id, selected } => {
                    let Some(panel) = state
                        .control
                        .pending_config_language_panel_mut()
                        .filter(|panel| panel.id == id)
                    else {
                        continue;
                    };
                    panel.selected_option = selected.min(2);
                    redraw_current_config_language_panel(state, output)?;
                }
                ConfigLanguageCardAction::Set { id, selected } => {
                    let Some(panel) = state
                        .control
                        .pending_config_language_panel()
                        .filter(|panel| panel.id == id)
                        .cloned()
                    else {
                        continue;
                    };
                    let selected = selected.min(2);
                    let pending_value = language_option_value(selected);
                    clear_active_config_language_panel(state, output)?;
                    state.control.clear_pending_config_language_panel();
                    let status = language_config_status();
                    if !begin_config_language_confirmation(state, &status, pending_value) {
                        render_config_home_missing(state, output)?;
                        write_shell_prompt(state, output)?;
                    } else {
                        render_current_config_panel(state, output)?;
                    }
                    let _ = panel;
                }
                ConfigLanguageCardAction::Cancel { id } => {
                    let Some(_panel) = state
                        .control
                        .pending_config_language_panel()
                        .filter(|panel| panel.id == id)
                    else {
                        continue;
                    };
                    clear_active_config_language_panel(state, output)?;
                    state.control.clear_pending_config_language_panel();
                    let i18n = state.i18n();
                    render_notice_panel(
                        output,
                        i18n.t(MessageId::ConfigUnchangedTitle),
                        vec![i18n.t(MessageId::ConfigNoFileChangedBody).to_string()],
                        None,
                    )?;
                    write_shell_prompt(state, output)?;
                }
            }
            output.flush()?;
            continue;
        }

        let Some(action) = config_card_action_from_event(event) else {
            continue;
        };
        let key = format!(
            "{}:{}",
            stable_event_key("config-card", event_index, event),
            event.message.as_deref().unwrap_or_default()
        );
        if !state.control.claim_config_action(key) {
            continue;
        }

        match action {
            ConfigCardAction::Focus { id, selected } => {
                let Some(panel) = state
                    .control
                    .pending_config_panel_mut()
                    .filter(|panel| panel.id == id)
                else {
                    continue;
                };
                panel.selected_option = selected.min(1);
                redraw_current_config_panel(state, output)?;
            }
            ConfigCardAction::Save { id } => {
                let Some(panel) = state
                    .control
                    .pending_config_panel()
                    .filter(|panel| panel.id == id)
                    .cloned()
                else {
                    continue;
                };
                clear_active_config_panel(state, output)?;
                state.control.clear_pending_config_panel();
                let result = write_user_language_config(&panel.pending_value);
                let (title, body, footer) = match result {
                    Ok(path) => {
                        state.language = parse_language_setting(&panel.pending_value)
                            .map(resolve_language_setting)
                            .unwrap_or(state.language);
                        let i18n = state.i18n();
                        (
                            i18n.t(MessageId::ConfigSavedTitle),
                            vec![
                                i18n.format(
                                    MessageId::ConfigSavedValueLine,
                                    &[("setting", &panel.setting), ("value", &panel.pending_value)],
                                ),
                                i18n.format(
                                    MessageId::ConfigCurrentSessionLanguageLine,
                                    &[("language", state.language.as_config_value())],
                                ),
                                i18n.format(
                                    MessageId::ConfigFileLine,
                                    &[("path", &path.display().to_string())],
                                ),
                            ],
                            i18n.t(MessageId::ConfigSavedFooter),
                        )
                    }
                    Err(err) => {
                        let i18n = state.i18n();
                        let err = err.to_string();
                        (
                            i18n.t(MessageId::ConfigSaveFailedTitle),
                            vec![i18n.format(MessageId::ConfigSaveFailedBody, &[("error", &err)])],
                            i18n.t(MessageId::ConfigNoFileChangedBody),
                        )
                    }
                };
                render_notice_panel(output, title, body, Some(footer))?;
                write_shell_prompt(state, output)?;
            }
            ConfigCardAction::Cancel { id } => {
                let Some(_panel) = state
                    .control
                    .pending_config_panel()
                    .filter(|panel| panel.id == id)
                else {
                    continue;
                };
                clear_active_config_panel(state, output)?;
                state.control.clear_pending_config_panel();
                let i18n = state.i18n();
                render_notice_panel(
                    output,
                    i18n.t(MessageId::ConfigUnchangedTitle),
                    vec![i18n.t(MessageId::ConfigNoFileChangedBody).to_string()],
                    None,
                )?;
                write_shell_prompt(state, output)?;
            }
        }
        output.flush()?;
    }
    Ok(())
}

fn render_current_config_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(panel) = state.control.pending_config_panel() else {
        return Ok(());
    };
    if state.control.active_config_panel_id() == Some(panel.id.as_str()) {
        return Ok(());
    }
    let marker = |i: usize| {
        if panel.selected_option == i {
            "> "
        } else {
            "  "
        }
    };
    let i18n = state.i18n();
    let body = vec![
        i18n.format(
            MessageId::ConfigFileLine,
            &[("path", &panel.config_path.display().to_string())],
        ),
        i18n.format(
            MessageId::ConfigPendingChangeLine,
            &[
                ("setting", &panel.setting),
                ("before", &panel.before_value),
                ("after", &panel.pending_value),
            ],
        ),
        format!("{}[ {}   ]", marker(0), i18n.t(MessageId::ConfigSaveButton)),
        format!("{}[ {} ]", marker(1), i18n.t(MessageId::ConfigCancelButton)),
    ];
    let footer = i18n.t(MessageId::ConfigApplyKeysFooter);
    render_notice_panel(
        output,
        i18n.t(MessageId::ConfigSavePromptTitle),
        body.clone(),
        Some(footer),
    )?;
    state
        .control
        .set_active_config_panel(panel.id.clone(), config_notice_height(&body, Some(footer)));
    Ok(())
}

fn redraw_current_config_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_config_panel(state, output)?;
    render_current_config_panel(state, output)
}

fn clear_active_config_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let height = state.control.active_config_panel_height();
    if height == 0 {
        state.control.clear_active_config_panel_id();
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
    state.control.clear_active_config_panel();
    Ok(())
}

fn render_current_config_language_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(panel) = state.control.pending_config_language_panel() else {
        return Ok(());
    };
    if state.control.active_config_language_panel_id() == Some(panel.id.as_str()) {
        return Ok(());
    }
    let marker = |i: usize| {
        if panel.selected_option == i {
            "> "
        } else {
            "  "
        }
    };
    let i18n = state.i18n();
    let body = vec![
        format!("{}{}", marker(0), i18n.t(MessageId::ConfigLanguageAutoLine)),
        format!("{}{}", marker(1), i18n.t(MessageId::ConfigLanguageEnLine)),
        format!("{}{}", marker(2), i18n.t(MessageId::ConfigLanguageZhLine)),
    ];
    let footer = i18n.t(MessageId::ConfigLanguageKeysFooter);
    render_notice_panel(
        output,
        i18n.t(MessageId::ConfigLanguageTitle),
        body.clone(),
        Some(footer),
    )?;
    state.control.set_active_config_language_panel(
        panel.id.clone(),
        config_notice_height(&body, Some(footer)),
    );
    Ok(())
}

fn redraw_current_config_language_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_config_language_panel(state, output)?;
    render_current_config_language_panel(state, output)
}

fn clear_active_config_language_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let height = state.control.active_config_language_panel_height();
    if height == 0 {
        state.control.clear_active_config_language_panel_id();
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
    state.control.clear_active_config_language_panel();
    Ok(())
}

fn config_notice_height(body: &[String], footer: Option<&str>) -> usize {
    let renderer = RatatuiInlineRenderer::for_terminal();
    let mut lines = body
        .iter()
        .flat_map(|line| renderer.markdown_text_lines(line))
        .collect::<Vec<_>>();
    if let Some(footer) = footer {
        lines.extend(renderer.markdown_text_lines(footer));
    }
    lines.len().max(1) + 2
}

enum ConfigCardAction {
    Focus { id: String, selected: usize },
    Save { id: String },
    Cancel { id: String },
}

enum ConfigLanguageCardAction {
    Focus { id: String, selected: usize },
    Set { id: String, selected: usize },
    Cancel { id: String },
}

fn config_language_card_action_from_event(event: &ShellEvent) -> Option<ConfigLanguageCardAction> {
    if event.kind != ShellEventKind::UserInputIntercepted
        || event.component.as_deref() != Some("card")
    {
        return None;
    }
    match event.message.as_deref()? {
        "config_language_focus" => {
            let (id, selected) = split_config_value(event.input.as_deref()?)?;
            Some(ConfigLanguageCardAction::Focus { id, selected })
        }
        "config_language_set" => {
            let (id, selected) = split_config_value(event.input.as_deref()?)?;
            Some(ConfigLanguageCardAction::Set { id, selected })
        }
        "config_language_cancel" => Some(ConfigLanguageCardAction::Cancel {
            id: event.input.as_deref()?.to_string(),
        }),
        _ => None,
    }
}

fn config_card_action_from_event(event: &ShellEvent) -> Option<ConfigCardAction> {
    if event.kind != ShellEventKind::UserInputIntercepted
        || event.component.as_deref() != Some("card")
    {
        return None;
    }
    match event.message.as_deref()? {
        "config_focus" => {
            let (id, selected) = split_config_value(event.input.as_deref()?)?;
            Some(ConfigCardAction::Focus { id, selected })
        }
        "config_save" => Some(ConfigCardAction::Save {
            id: event.input.as_deref()?.to_string(),
        }),
        "config_cancel" => Some(ConfigCardAction::Cancel {
            id: event.input.as_deref()?.to_string(),
        }),
        _ => None,
    }
}

fn split_config_value(value: &str) -> Option<(String, usize)> {
    let (id, selected) = value.split_once(':')?;
    Some((id.to_string(), selected.parse().ok()?))
}

fn language_option_index(setting: &str) -> usize {
    match setting {
        "en-US" => 1,
        "zh-CN" => 2,
        _ => 0,
    }
}

fn language_option_value(index: usize) -> &'static str {
    match index {
        1 => "en-US",
        2 => "zh-CN",
        _ => "auto",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zh_state() -> InlineState {
        InlineState {
            language: Language::ZhCn,
            ..InlineState::default()
        }
    }

    #[test]
    fn config_invalid_language_uses_zh_catalog_text() {
        let mut state = zh_state();
        let mut output = Vec::new();

        render_config_command(Some("language"), Some("fr-FR"), &mut state, &mut output)
            .expect("render invalid language");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("无效语言: fr-FR"), "{output}");
        assert!(output.contains("支持: auto, en-US, zh-CN。"), "{output}");
        assert!(!output.contains("Invalid language"), "{output}");
    }

    #[test]
    fn config_language_panel_uses_zh_catalog_text() {
        let mut state = zh_state();
        state.control.set_pending_config_language_panel(2);
        let mut output = Vec::new();

        render_current_config_language_panel(&mut state, &mut output)
            .expect("render language panel");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("语言"), "{output}");
        assert!(output.contains("跟随 LC_ALL/LC_MESSAGES/LANG"), "{output}");
        assert!(output.contains("简体中文"), "{output}");
        assert!(output.contains("按键:"), "{output}");
        assert!(!output.contains("Simplified Chinese"), "{output}");
    }

    #[test]
    fn config_save_panel_uses_zh_catalog_text() {
        let mut state = zh_state();
        let status = LanguageConfigStatus {
            setting: "auto".to_string(),
            effective: Language::ZhCn,
            source: "config",
            config_path: Some(std::env::temp_dir().join("cosh-shell-config-test.toml")),
        };
        assert!(begin_config_language_confirmation(
            &mut state, &status, "zh-CN"
        ));
        let mut output = Vec::new();

        render_current_config_panel(&mut state, &mut output).expect("render save panel");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("保存配置？"), "{output}");
        assert!(output.contains("[ 保存"), "{output}");
        assert!(output.contains("[ 取消"), "{output}");
        assert!(output.contains("按键:"), "{output}");
        assert!(!output.contains("Save config?"), "{output}");
    }

    #[test]
    fn config_cancel_notice_uses_zh_catalog_text() {
        let mut state = zh_state();
        let status = LanguageConfigStatus {
            setting: "auto".to_string(),
            effective: Language::ZhCn,
            source: "config",
            config_path: Some(std::env::temp_dir().join("cosh-shell-config-test.toml")),
        };
        assert!(begin_config_language_confirmation(
            &mut state, &status, "zh-CN"
        ));
        let panel_id = state
            .control
            .pending_config_panel()
            .expect("pending panel")
            .id
            .clone();
        let mut event = ShellEvent::user_input_intercepted("session", &panel_id);
        event.component = Some("card".to_string());
        event.message = Some("config_cancel".to_string());
        let mut output = Vec::new();

        render_config_card_actions(&[event], &mut state, &mut output, 0).expect("render cancel");

        let output = String::from_utf8(output).expect("utf8 output");
        assert!(output.contains("配置未变更"), "{output}");
        assert!(output.contains("未修改配置文件。"), "{output}");
        assert!(!output.contains("Config unchanged"), "{output}");
    }
}
