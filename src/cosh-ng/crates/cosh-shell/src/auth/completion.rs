use crate::recommendation::personal_session::request_retry_after_auth;
use crate::runtime::prelude::{NoticePanelModel, RatatuiInlineRenderer};
use crate::runtime::state::InlineState;

pub(super) fn finish_auth_configuration<W: std::io::Write>(
    state: &mut InlineState,
    output: &mut W,
    provider_label: &str,
) -> std::io::Result<()> {
    request_retry_after_auth(state);

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
