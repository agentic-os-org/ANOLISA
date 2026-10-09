use crate::runtime::prelude::*;

/// `/plan` (#1776): a bare command toggles plan mode — the first entry
/// starts read-only investigation, a second `/plan` exits it. Explicit
/// values and the status view come through `/mode plan [on|off|status]`.
pub(crate) fn render_plan_command<W: Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    set_plan_mode(state, !state.plan_mode, output)
}

pub(crate) fn render_plan_mode_command<W: Write>(
    arg: Option<&str>,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<bool> {
    match arg {
        None | Some("status") => render_notice_panel(
            output,
            state.i18n().t(MessageId::PlanModeTitle),
            vec![state.i18n().format(
                MessageId::PlanModeCurrentBody,
                &[("mode", plan_mode_label(state))],
            )],
            Some(plan_mode_footer(state.i18n(), state.plan_mode)),
        )
        .map(|_| true),
        Some("on") => set_plan_mode(state, true, output),
        Some("off") => set_plan_mode(state, false, output),
        Some(other) => render_notice_panel(
            output,
            state.i18n().t(MessageId::PlanModeTitle),
            vec![state
                .i18n()
                .format(MessageId::PlanModeUnknownBody, &[("mode", other)])],
            Some(state.i18n().t(MessageId::PlanModeUsageFooter)),
        )
        .map(|_| true),
    }
}

fn set_plan_mode<W: Write>(
    state: &mut InlineState,
    enabled: bool,
    output: &mut W,
) -> std::io::Result<bool> {
    state.plan_mode = enabled;
    render_notice_panel(
        output,
        state.i18n().t(MessageId::PlanModeTitle),
        vec![state.i18n().format(
            MessageId::PlanModeSetBody,
            &[("mode", plan_mode_label(state))],
        )],
        Some(plan_mode_footer(state.i18n(), state.plan_mode)),
    )?;
    Ok(true)
}

pub(crate) fn plan_mode_label(state: &InlineState) -> &'static str {
    if state.plan_mode {
        "on"
    } else {
        "off"
    }
}

fn plan_mode_footer(i18n: I18n, enabled: bool) -> &'static str {
    if enabled {
        i18n.t(MessageId::PlanModeOnFooter)
    } else {
        i18n.t(MessageId::PlanModeOffFooter)
    }
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

#[cfg(test)]
mod tests {
    use super::{plan_mode_label, render_plan_command, render_plan_mode_command};
    use crate::runtime::state::InlineState;

    #[test]
    fn plan_command_toggles_plan_mode_and_reports_the_exit_path() {
        let mut state = InlineState::default();
        let mut output = Vec::new();

        assert!(!state.plan_mode);
        assert!(render_plan_command(&mut state, &mut output).expect("enter plan mode"));
        assert!(state.plan_mode);
        let entered = String::from_utf8_lossy(&output);
        assert!(entered.contains("on"), "entered panel shows on: {entered}");
        assert!(entered.contains("/plan"), "entered panel points at exit");

        output.clear();
        assert!(render_plan_command(&mut state, &mut output).expect("exit plan mode"));
        assert!(!state.plan_mode);
        let exited = String::from_utf8_lossy(&output);
        assert!(exited.contains("off"), "exited panel shows off: {exited}");
    }

    #[test]
    fn plan_mode_command_sets_reads_and_rejects_unknown_values() {
        let mut state = InlineState::default();
        let mut output = Vec::new();

        assert!(
            render_plan_mode_command(Some("on"), &mut state, &mut output).expect("explicit on")
        );
        assert!(state.plan_mode);

        output.clear();
        assert!(
            render_plan_mode_command(Some("status"), &mut state, &mut output).expect("status view")
        );
        let status = String::from_utf8_lossy(&output);
        assert!(status.contains("on"), "status shows on: {status}");
        assert!(state.plan_mode, "status must not change the mode");

        output.clear();
        assert!(
            render_plan_mode_command(Some("off"), &mut state, &mut output).expect("explicit off")
        );
        assert!(!state.plan_mode);

        output.clear();
        assert!(
            render_plan_mode_command(Some("maybe"), &mut state, &mut output)
                .expect("unknown value notice")
        );
        assert!(!state.plan_mode);
        let unknown = String::from_utf8_lossy(&output);
        assert!(unknown.contains("maybe"), "unknown value echoed: {unknown}");
        assert!(
            unknown.contains("/mode plan on|off|status"),
            "usage footer: {unknown}"
        );
    }

    #[test]
    fn plan_mode_label_tracks_state() {
        let mut state = InlineState::default();
        assert_eq!(plan_mode_label(&state), "off");
        state.plan_mode = true;
        assert_eq!(plan_mode_label(&state), "on");
    }
}
