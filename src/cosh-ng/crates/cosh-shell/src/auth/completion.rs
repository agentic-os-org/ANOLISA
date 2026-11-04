use crate::recommendation::personal_session::request_retry_after_auth;
use crate::runtime::prelude::{NoticePanelModel, RatatuiInlineRenderer};
use crate::runtime::state::InlineState;

pub(super) fn finish_auth_configuration<W: std::io::Write>(
    state: &mut InlineState,
    output: &mut W,
    provider_label: &str,
) -> std::io::Result<()> {
    if !state.auth.ecs_trial.cancelled_question {
        request_retry_after_auth(state);
    }

    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: "Auth configured",
            body: vec![format!(
                "Provider: {provider_label} \u{2014} credentials saved."
            )],
            footer: None,
        },
    )?;

    crate::slash::prompt::write_shell_prompt(state, output)?;

    output.flush()
}

pub(super) fn finish_partial_configuration<W: std::io::Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    super::runtime::clear_observed_model_after_provider_change(state);
    if let Some(auth) = state.auth.state.take() {
        state.auth.completed_ids.insert(auth.id);
    }
    super::prompt::clear_active_auth_panel(state, output)?;
    super::trial::notice(state, output,
        "模型配置已保存，但会话未能保存。可继续提问；后续提问时会再次尝试保存会话。",
        "Model configuration saved, but the session could not be saved. You can ask another question; session saving will be retried on that turn.")?;
    crate::slash::prompt::write_shell_prompt(state, output)?;
    output.flush()
}
