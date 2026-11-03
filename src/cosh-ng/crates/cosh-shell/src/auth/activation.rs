use std::io::{self, Write};

use crate::adapter::AdapterInstance;
use crate::runtime::prelude::{NoticePanelModel, RatatuiInlineRenderer};
use crate::runtime::state::InlineState;

use super::prompt::{clear_active_auth_panel, render_current_auth_panel};
use super::provider_management::{core_auth_activate, ExistingProvider};
use super::runtime::clear_observed_model_after_provider_change;

pub(super) fn activate_provider<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    existing: &ExistingProvider,
    output: &mut W,
) -> io::Result<()> {
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    if let Err(error) = core_auth_activate(adapter, &existing.name) {
        tracing::warn!(provider_id = %existing.name, error = ?error, "provider activation failed");
        if let Some(auth) = state.auth.state.as_mut() {
            auth.field_capture_revision = auth.field_capture_revision.wrapping_add(1);
        }
        clear_active_auth_panel(state, output)?;
        renderer.write_notice_panel(
            output,
            NoticePanelModel {
                title: "Provider switch not confirmed",
                body: vec![error, "Review the error and retry when ready.".into()],
                footer: None,
            },
        )?;
        render_current_auth_panel(state, output)?;
        return output.flush();
    }
    clear_observed_model_after_provider_change(state);
    state.auth.state.take();
    clear_active_auth_panel(state, output)?;
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: "Provider switched",
            body: vec![format!(
                "Active provider: {} (\"{}\")",
                existing.label, existing.name
            )],
            footer: None,
        },
    )?;
    crate::slash::prompt::write_shell_prompt(state, output)?;
    output.flush()
}
