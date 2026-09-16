//! Active-run auth request state and validation-error rendering.

use std::collections::HashMap;
use std::io::{self, Write};

use crate::runtime::prelude::{AgentEvent, GovernedEvent, NoticePanelModel, RatatuiInlineRenderer};
use crate::runtime::state::InlineState;

use super::menu::SysomMenu;
use super::prompt::render_current_auth_panel;
use super::provider_display::auth_providers_for_display;
use super::runtime::{AuthBackend, AuthPhase, RuntimeAuthState};

pub(crate) fn record_auth_required(
    state: &mut InlineState,
    governed_events: &[GovernedEvent],
) -> Vec<String> {
    let mut ids = Vec::new();
    for event in governed_events {
        if let AgentEvent::AuthRequired {
            request_id,
            error_message,
            providers,
            ..
        } = &event.event
        {
            if state.auth.state.is_some() {
                continue;
            }
            let id = format!("auth-{request_id}");
            if state.auth.completed_ids.contains(&id) {
                continue;
            }
            state.auth.state = Some(RuntimeAuthState {
                id: id.clone(),
                request_id: request_id.clone(),
                phase: AuthPhase::SelectingProvider,
                providers: auth_providers_for_display(providers),
                selected_provider: 0,
                current_field: 0,
                collected_values: HashMap::new(),
                field_input: String::new(),
                field_error: None,
                field_capture_revision: 0,
                existing_providers: Vec::new(),
                editing_provider_name: None,
                default_provider_id: false,
                from_sysom_shortcut: false,
                error_message: error_message.clone(),
                backend: AuthBackend::ActiveRun,
                // The active-run flow never shows the management menu.
                sysom: SysomMenu::default(),
            });
            ids.push(id);
        }
    }
    ids
}

pub(crate) fn render_auth_panel<W: Write>(
    state: &mut InlineState,
    ids: &[String],
    output: &mut W,
) -> io::Result<()> {
    for id in ids {
        let Some(auth) = state.auth.state.as_ref().filter(|auth| auth.id == *id) else {
            continue;
        };
        if let Some(error) = auth.error_message.clone() {
            let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
            renderer.write_notice_panel(
                output,
                NoticePanelModel {
                    title: "Credentials were not saved",
                    body: vec![error, "Review the values and try again.".to_string()],
                    footer: None,
                },
            )?;
        }
        render_current_auth_panel(state, output)?;
    }
    Ok(())
}
