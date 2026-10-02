use crate::runtime::prelude::*;
use crate::runtime::state::RuntimeModePanelKind;
use crate::slash::prompt::write_shell_prompt;

pub(crate) fn render_mode_command<W: Write>(
    arg: Option<&str>,
    sub: Option<&str>,
    confirm: Option<&str>,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    match arg {
        None => render_mode_summary(state, output),
        Some("approval") => render_approval_mode_command(sub, confirm, state, output),
        Some("analysis") => render_analysis_mode_command(sub, state, output),
        Some("routing") => render_routing_mode_command(sub, state, output),
        Some("recommend" | "auto" | "trust") => render_notice_panel(
            output,
            state.i18n().t(MessageId::ModeRemovedTitle),
            vec![state
                .i18n()
                .format(MessageId::ModeRemovedBody, &[("mode", arg.unwrap())])],
            Some(
                &state
                    .i18n()
                    .format(MessageId::ModeRemovedFooter, &[("mode", arg.unwrap())]),
            ),
        )
        .map(|_| true),
        Some("language") => render_notice_panel(
            output,
            state.i18n().t(MessageId::ModeTitle),
            vec![state.i18n().t(MessageId::ModeLanguageBody).to_string()],
            Some(state.i18n().t(MessageId::ModeLanguageFooter)),
        )
        .map(|_| true),
        Some(other) => render_notice_panel(
            output,
            state.i18n().t(MessageId::ModeTitle),
            vec![state
                .i18n()
                .format(MessageId::ModeUnknownBody, &[("mode", other)])],
            Some(state.i18n().t(MessageId::ModeUnknownFooter)),
        )
        .map(|_| true),
    }
}

fn render_mode_summary<W: Write>(state: &InlineState, output: &mut W) -> std::io::Result<bool> {
    render_notice_panel(
        output,
        state.i18n().t(MessageId::ModesTitle),
        vec![
            state.i18n().format(
                MessageId::ModeApprovalLine,
                &[("mode", state.approval_mode.label())],
            ),
            state.i18n().format(
                MessageId::ModeAnalysisLine,
                &[("mode", state.analysis_mode.label())],
            ),
            state.i18n().format(
                MessageId::ModeRoutingLine,
                &[("mode", routing_mode_label(state))],
            ),
        ],
        Some(state.i18n().t(MessageId::ModeSummaryFooter)),
    )?;
    Ok(true)
}

fn render_routing_mode_command<W: Write>(
    arg: Option<&str>,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    let Some(control) = state.assistance_control.clone() else {
        render_notice_panel(
            output,
            state.i18n().t(MessageId::RoutingModeTitle),
            vec![state
                .i18n()
                .t(MessageId::RoutingModeUnavailableBody)
                .to_string()],
            Some(state.i18n().t(MessageId::RoutingModeUsageFooter)),
        )?;
        return Ok(true);
    };

    match arg {
        None => render_notice_panel(
            output,
            state.i18n().t(MessageId::RoutingModeTitle),
            vec![state.i18n().format(
                MessageId::RoutingModeCurrentBody,
                &[("mode", routing_mode_label(state))],
            )],
            Some(routing_mode_footer(state)),
        )
        .map(|_| true),
        Some("assisted") => set_routing_mode(true, &control, state, output),
        Some("shell-only") => set_routing_mode(false, &control, state, output),
        Some(other) => render_notice_panel(
            output,
            state.i18n().t(MessageId::RoutingModeTitle),
            vec![state
                .i18n()
                .format(MessageId::RoutingModeUnknownBody, &[("mode", other)])],
            Some(state.i18n().t(MessageId::RoutingModeUsageFooter)),
        )
        .map(|_| true),
    }
}

fn set_routing_mode<W: Write>(
    enabled: bool,
    control: &crate::input::AssistanceControl,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    control.set_enabled(enabled)?;
    let mode = routing_mode_label(state);
    render_notice_panel(
        output,
        state.i18n().t(MessageId::RoutingModeTitle),
        vec![state
            .i18n()
            .format(MessageId::RoutingModeSetBody, &[("mode", mode)])],
        Some(routing_mode_footer(state)),
    )?;
    Ok(true)
}

fn routing_mode_label(state: &InlineState) -> &'static str {
    match state.assistance_control.as_ref() {
        Some(control) if control.is_enabled() => "assisted",
        Some(_) => "shell-only",
        None => "native",
    }
}

fn routing_mode_footer(state: &InlineState) -> &'static str {
    if state
        .assistance_control
        .as_ref()
        .is_some_and(crate::input::AssistanceControl::is_enabled)
    {
        state.i18n().t(MessageId::RoutingModeAssistedFooter)
    } else {
        state.i18n().t(MessageId::RoutingModeShellOnlyFooter)
    }
}

fn render_approval_mode_command<W: Write>(
    arg: Option<&str>,
    confirm: Option<&str>,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    match arg {
        None => {
            state.control.set_pending_mode_panel(
                RuntimeModePanelKind::Approval,
                match state.approval_mode {
                    CoshApprovalMode::Recommend => 0,
                    CoshApprovalMode::Auto => 1,
                    CoshApprovalMode::Trust => 2,
                },
            );
            render_current_mode_panel(state, output)?;
            Ok(false)
        }
        Some("recommend") => {
            state.approval_mode = CoshApprovalMode::Recommend;
            render_notice_panel(
                output,
                state.i18n().t(MessageId::ApprovalModeTitle),
                vec![state
                    .i18n()
                    .format(MessageId::ApprovalModeSetBody, &[("mode", "recommend")])],
                Some(mode_footer(state.i18n(), CoshApprovalMode::Recommend)),
            )?;
            Ok(true)
        }
        Some("auto") => {
            state.approval_mode = CoshApprovalMode::Auto;
            render_notice_panel(
                output,
                state.i18n().t(MessageId::ApprovalModeTitle),
                vec![state
                    .i18n()
                    .format(MessageId::ApprovalModeSetBody, &[("mode", "auto")])],
                Some(mode_footer(state.i18n(), CoshApprovalMode::Auto)),
            )?;
            Ok(true)
        }
        Some("trust") if confirm != Some("confirm") => {
            render_trust_confirmation_required(state.i18n(), output)
        }
        Some("trust") => {
            state.approval_mode = CoshApprovalMode::Trust;
            render_notice_panel(
                output,
                state.i18n().t(MessageId::ApprovalModeTitle),
                vec![state
                    .i18n()
                    .format(MessageId::ApprovalModeSetBody, &[("mode", "trust")])],
                Some(mode_footer(state.i18n(), CoshApprovalMode::Trust)),
            )?;
            Ok(true)
        }
        Some(other) => render_notice_panel(
            output,
            state.i18n().t(MessageId::ApprovalModeTitle),
            vec![state
                .i18n()
                .format(MessageId::ApprovalModeUnknownBody, &[("mode", other)])],
            Some(state.i18n().t(MessageId::ApprovalModeUsageFooter)),
        )
        .map(|_| true),
    }
}

fn render_trust_confirmation_required<W: Write>(
    i18n: I18n,
    output: &mut W,
) -> std::io::Result<bool> {
    render_notice_panel(
        output,
        i18n.t(MessageId::ApprovalModeTrustConfirmationTitle),
        vec![
            i18n.t(MessageId::ApprovalModeTrustConfirmationBody)
                .to_string(),
            i18n.t(MessageId::ApprovalModeTrustConfirmationCommandBody)
                .to_string(),
        ],
        Some(i18n.t(MessageId::ApprovalModeTrustConfirmationFooter)),
    )?;
    Ok(true)
}

fn render_analysis_mode_command<W: Write>(
    arg: Option<&str>,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    match arg {
        None => {
            state.control.set_pending_mode_panel(
                RuntimeModePanelKind::Analysis,
                match state.analysis_mode {
                    AnalysisMode::Smart => 0,
                    AnalysisMode::Auto => 1,
                    AnalysisMode::Manual => 2,
                },
            );
            render_current_mode_panel(state, output)?;
            Ok(false)
        }
        Some("smart") => {
            set_analysis_mode(state, AnalysisMode::Smart);
            render_notice_panel(
                output,
                state.i18n().t(MessageId::AnalysisModeTitle),
                vec![state
                    .i18n()
                    .format(MessageId::AnalysisModeSetBody, &[("mode", "smart")])],
                Some(state.i18n().t(MessageId::AnalysisModeSmartFooter)),
            )?;
            Ok(true)
        }
        Some("auto") => {
            set_analysis_mode(state, AnalysisMode::Auto);
            render_notice_panel(
                output,
                state.i18n().t(MessageId::AnalysisModeTitle),
                vec![state
                    .i18n()
                    .format(MessageId::AnalysisModeSetBody, &[("mode", "auto")])],
                Some(state.i18n().t(MessageId::AnalysisModeAutoFooter)),
            )?;
            Ok(true)
        }
        Some("manual") => {
            set_analysis_mode(state, AnalysisMode::Manual);
            render_notice_panel(
                output,
                state.i18n().t(MessageId::AnalysisModeTitle),
                vec![state
                    .i18n()
                    .format(MessageId::AnalysisModeSetBody, &[("mode", "manual")])],
                Some(state.i18n().t(MessageId::AnalysisModeManualFooter)),
            )?;
            Ok(true)
        }
        Some(other) => render_notice_panel(
            output,
            state.i18n().t(MessageId::AnalysisModeTitle),
            vec![state
                .i18n()
                .format(MessageId::AnalysisModeUnknownBody, &[("mode", other)])],
            Some(state.i18n().t(MessageId::AnalysisModeUsageFooter)),
        )
        .map(|_| true),
    }
}

pub(crate) fn render_mode_card_actions<W: Write>(
    events: &[ShellEvent],
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        let Some(action) = mode_card_action_from_event(event) else {
            continue;
        };
        let key = format!(
            "{}:{}",
            stable_event_key("mode-card", event_index, event),
            event.message.as_deref().unwrap_or_default()
        );
        if !state.control.claim_mode_action(key) {
            continue;
        }

        match action {
            ModeCardAction::Focus { id, selected } => {
                let Some(panel) = state
                    .control
                    .pending_mode_panel_mut()
                    .filter(|panel| panel.id == id)
                else {
                    continue;
                };
                panel.selected_option = selected.min(2);
                redraw_current_mode_panel(state, output)?;
            }
            ModeCardAction::Set { id, selected } => {
                let Some(kind) = state
                    .control
                    .pending_mode_panel()
                    .filter(|panel| panel.id == id)
                    .map(|panel| panel.kind)
                else {
                    continue;
                };
                match kind {
                    RuntimeModePanelKind::Approval => {
                        apply_approval_mode_selection(selected, state, output)?
                    }
                    RuntimeModePanelKind::Analysis => {
                        apply_analysis_mode_selection(selected, state, output)?
                    }
                }
            }
            ModeCardAction::Cancel { id } => {
                let Some(kind) = state
                    .control
                    .pending_mode_panel()
                    .filter(|panel| panel.id == id)
                    .map(|panel| panel.kind)
                else {
                    continue;
                };
                clear_active_mode_panel(state, output)?;
                state.control.clear_pending_mode_panel();
                render_mode_cancel_notice(kind, state, output)?;
                write_shell_prompt(state, output)?;
            }
        }
        output.flush()?;
    }
    Ok(())
}

fn apply_approval_mode_selection<W: Write>(
    selected: usize,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let mode = approval_mode_from_index(selected.min(2));
    if mode == CoshApprovalMode::Trust && state.approval_mode != CoshApprovalMode::Trust {
        clear_active_mode_panel(state, output)?;
        state.control.clear_pending_mode_panel();
        render_trust_confirmation_required(state.i18n(), output)?;
        write_shell_prompt(state, output)?;
        return Ok(());
    }

    let unchanged = mode == state.approval_mode;
    state.approval_mode = mode;
    let label = state.approval_mode.label();
    clear_active_mode_panel(state, output)?;
    state.control.clear_pending_mode_panel();
    let message = if unchanged {
        MessageId::ApprovalModeRemainsBody
    } else {
        MessageId::ApprovalModeSetBody
    };
    render_notice_panel(
        output,
        state.i18n().t(MessageId::ApprovalModeCardTitle),
        vec![state.i18n().format(message, &[("mode", label)])],
        Some(mode_footer(state.i18n(), state.approval_mode)),
    )?;
    write_shell_prompt(state, output)
}

fn apply_analysis_mode_selection<W: Write>(
    selected: usize,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let mode = analysis_mode_from_index(selected.min(2));
    let unchanged = mode == state.analysis_mode;
    set_analysis_mode(state, mode);
    let label = state.analysis_mode.label();
    clear_active_mode_panel(state, output)?;
    state.control.clear_pending_mode_panel();
    let message = if unchanged {
        MessageId::AnalysisModeRemainsBody
    } else {
        MessageId::AnalysisModeSetBody
    };
    render_notice_panel(
        output,
        state.i18n().t(MessageId::AnalysisModeTitle),
        vec![state.i18n().format(message, &[("mode", label)])],
        Some(analysis_mode_footer(state.i18n(), state.analysis_mode)),
    )?;
    write_shell_prompt(state, output)
}

fn set_analysis_mode(state: &mut InlineState, mode: AnalysisMode) {
    state.analysis_mode = mode;
    if mode != AnalysisMode::Manual {
        return;
    }
    if let Some(cancellation) = state.personalization.analyzer_cancellation.as_ref() {
        cancellation.cancel_current();
    }
    state.personalization.analyzer_started = false;
    state.clear_personal_prompt_ghost();
    state.pending_input_ghost = None;
    state.pending_input_ghost_route = Default::default();
    state.pending_input_ghost_binding = None;
}

fn render_mode_cancel_notice<W: Write>(
    kind: RuntimeModePanelKind,
    state: &InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let (title, message, footer, label) = match kind {
        RuntimeModePanelKind::Approval => (
            MessageId::ApprovalModeCardTitle,
            MessageId::ApprovalModeCancelBody,
            MessageId::ApprovalModeCancelFooter,
            state.approval_mode.label(),
        ),
        RuntimeModePanelKind::Analysis => (
            MessageId::AnalysisModeTitle,
            MessageId::AnalysisModeCancelBody,
            MessageId::AnalysisModeCancelFooter,
            state.analysis_mode.label(),
        ),
    };
    render_notice_panel(
        output,
        state.i18n().t(title),
        vec![state.i18n().format(message, &[("mode", label)])],
        Some(state.i18n().t(footer)),
    )
}

fn render_current_mode_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(panel) = state.control.pending_mode_panel() else {
        return Ok(());
    };
    if state.control.active_mode_panel_id() == Some(panel.id.as_str()) {
        return Ok(());
    }

    let marker = |i: usize| {
        if panel.selected_option == i {
            "> "
        } else {
            "  "
        }
    };
    let (title, body, footer) = match panel.kind {
        RuntimeModePanelKind::Approval => (
            state.i18n().t(MessageId::ApprovalModeCardTitle),
            vec![
                state.i18n().format(
                    MessageId::ApprovalModeCardCurrentLine,
                    &[("mode", state.approval_mode.label())],
                ),
                state.i18n().format(
                    MessageId::ApprovalModeCardRecommendLine,
                    &[("marker", marker(0))],
                ),
                state.i18n().format(
                    MessageId::ApprovalModeCardAutoLine,
                    &[("marker", marker(1))],
                ),
                state.i18n().format(
                    MessageId::ApprovalModeCardTrustLine,
                    &[("marker", marker(2))],
                ),
            ],
            state.i18n().t(MessageId::ApprovalModeCardFooter),
        ),
        RuntimeModePanelKind::Analysis => (
            state.i18n().t(MessageId::AnalysisModeTitle),
            vec![
                state.i18n().format(
                    MessageId::AnalysisModeCurrentBody,
                    &[("mode", state.analysis_mode.label())],
                ),
                state.i18n().format(
                    MessageId::AnalysisModeCardSmartLine,
                    &[("marker", marker(0))],
                ),
                state.i18n().format(
                    MessageId::AnalysisModeCardAutoLine,
                    &[("marker", marker(1))],
                ),
                state.i18n().format(
                    MessageId::AnalysisModeCardManualLine,
                    &[("marker", marker(2))],
                ),
            ],
            state.i18n().t(MessageId::AnalysisModeCardFooter),
        ),
    };
    render_notice_panel(output, title, body.clone(), Some(footer))?;
    state
        .control
        .set_active_mode_panel(panel.id.clone(), notice_height(&body, Some(footer)));
    Ok(())
}

fn redraw_current_mode_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_mode_panel(state, output)?;
    render_current_mode_panel(state, output)
}

fn clear_active_mode_panel<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let height = state.control.active_mode_panel_height();
    if height == 0 {
        state.control.clear_active_mode_panel_id();
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
    state.control.clear_active_mode_panel();
    Ok(())
}

enum ModeCardAction {
    Focus { id: String, selected: usize },
    Set { id: String, selected: usize },
    Cancel { id: String },
}

fn mode_card_action_from_event(event: &ShellEvent) -> Option<ModeCardAction> {
    if event.kind != ShellEventKind::UserInputIntercepted
        || event.component.as_deref() != Some("card")
    {
        return None;
    }

    match event.message.as_deref()? {
        "mode_focus" => {
            let (id, selected) = split_mode_value(event.input.as_deref()?)?;
            Some(ModeCardAction::Focus { id, selected })
        }
        "mode_set" => {
            let (id, selected) = split_mode_value(event.input.as_deref()?)?;
            Some(ModeCardAction::Set { id, selected })
        }
        "mode_cancel" => Some(ModeCardAction::Cancel {
            id: event.input.as_deref()?.to_string(),
        }),
        _ => None,
    }
}

fn split_mode_value(value: &str) -> Option<(String, usize)> {
    let (id, selected) = value.split_once(':')?;
    Some((id.to_string(), selected.parse().ok()?))
}

fn approval_mode_from_index(index: usize) -> CoshApprovalMode {
    match index {
        0 => CoshApprovalMode::Recommend,
        1 => CoshApprovalMode::Auto,
        2 => CoshApprovalMode::Trust,
        _ => CoshApprovalMode::Recommend,
    }
}

fn analysis_mode_from_index(index: usize) -> AnalysisMode {
    match index {
        0 => AnalysisMode::Smart,
        1 => AnalysisMode::Auto,
        2 => AnalysisMode::Manual,
        _ => AnalysisMode::Smart,
    }
}

fn mode_footer(i18n: I18n, mode: CoshApprovalMode) -> &'static str {
    match mode {
        CoshApprovalMode::Recommend => i18n.t(MessageId::ApprovalModeRecommendFooter),
        CoshApprovalMode::Auto => i18n.t(MessageId::ApprovalModeAutoFooter),
        CoshApprovalMode::Trust => i18n.t(MessageId::ApprovalModeTrustFooter),
    }
}

fn analysis_mode_footer(i18n: I18n, mode: AnalysisMode) -> &'static str {
    match mode {
        AnalysisMode::Smart => i18n.t(MessageId::AnalysisModeSmartFooter),
        AnalysisMode::Auto => i18n.t(MessageId::AnalysisModeAutoFooter),
        AnalysisMode::Manual => i18n.t(MessageId::AnalysisModeManualFooter),
    }
}

fn notice_height(body: &[String], footer: Option<&str>) -> usize {
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

fn render_notice_panel<W: Write>(
    output: &mut W,
    title: &str,
    body: Vec<String>,
    footer: Option<&str>,
) -> std::io::Result<()> {
    RatatuiInlineRenderer::for_terminal().write_notice_panel(
        output,
        NoticePanelModel {
            title,
            body,
            footer,
        },
    )
}
