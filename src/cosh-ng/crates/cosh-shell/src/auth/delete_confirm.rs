//! Destructive provider-delete confirmation and result rendering.

use std::io::{self, Write};

use crate::adapter::AdapterInstance;
use crate::runtime::prelude::{
    NoticePanelModel, QuestionInputFeedback, QuestionPanelModel, QuestionSelectionMode,
    RatatuiInlineRenderer,
};

use super::provider_management::{core_auth_delete, load_core_auth_state};
use super::runtime::{AuthPhase, RuntimeAuthState};

pub(super) const DELETE_CONFIRM_OPTION_COUNT: usize = 2;
const DELETE_OPTION_INDEX: usize = 1;

pub(super) fn begin_delete_confirmation(auth: &mut RuntimeAuthState, provider_idx: usize) {
    auth.phase = AuthPhase::ConfirmDelete { provider_idx };
    auth.selected_provider = 0;
}

pub(super) fn focus_delete_confirmation(auth: &mut RuntimeAuthState, selected: usize) {
    auth.selected_provider = selected.min(DELETE_CONFIRM_OPTION_COUNT - 1);
}

pub(super) enum DeleteConfirmationOutcome {
    Cancelled,
    Deleted {
        provider_name: String,
        needs_reselection: bool,
    },
}

pub(super) fn submit_delete_confirmation(
    adapter: &AdapterInstance,
    auth: &mut RuntimeAuthState,
    provider_idx: usize,
) -> Result<DeleteConfirmationOutcome, String> {
    if auth.selected_provider != DELETE_OPTION_INDEX {
        auth.phase = AuthPhase::ProviderAction { provider_idx };
        auth.selected_provider = 0;
        return Ok(DeleteConfirmationOutcome::Cancelled);
    }

    let existing = auth
        .existing_providers
        .get(provider_idx)
        .cloned()
        .ok_or_else(|| "provider selection is no longer valid".to_string())?;
    core_auth_delete(adapter, &existing.name)?;
    let AdapterInstance::CoshCore(cosh_core) = adapter else {
        return Err("auth registry requires cosh-core backend".to_string());
    };
    let core_state = load_core_auth_state(cosh_core)?;
    auth.existing_providers = core_state.existing_providers;
    // Re-derive the SysOM slot from the reloaded list: deleting the promoted RAM-role
    // provider must bring the shortcut back rather than relabel its replacement.
    auth.sysom.sync(&mut auth.existing_providers);
    auth.phase = AuthPhase::ManagingProviders;
    auth.selected_provider = 0;
    let needs_reselection = existing.is_active
        && !auth
            .existing_providers
            .iter()
            .any(|provider| provider.is_active);

    Ok(DeleteConfirmationOutcome::Deleted {
        provider_name: existing.name,
        needs_reselection,
    })
}

pub(super) fn render_delete_confirmation<W: Write>(
    auth: &RuntimeAuthState,
    provider_idx: usize,
    panel_id: &str,
    renderer: &RatatuiInlineRenderer,
    output: &mut W,
) -> io::Result<usize> {
    let provider = auth
        .existing_providers
        .get(provider_idx)
        .ok_or_else(|| io::Error::other("provider selection is no longer valid"))?;
    let question = format!(
        "Delete provider \"{}\"?\nThis removes its saved credentials and cannot be undone.",
        provider.name
    );
    let options = vec!["Cancel".to_string(), "Delete provider".to_string()];
    renderer.write_question_panel(
        output,
        QuestionPanelModel {
            id: panel_id,
            question: &question,
            options: &options,
            selected_option: auth.selected_provider,
            selected_options: &[],
            custom_answer: "",
            allow_free_text: false,
            selection_mode: QuestionSelectionMode::Single,
            input_feedback: QuestionInputFeedback::Disabled,
        },
    )
}

pub(super) fn render_delete_outcome<W: Write>(
    outcome: &DeleteConfirmationOutcome,
    renderer: &RatatuiInlineRenderer,
    output: &mut W,
) -> io::Result<()> {
    let DeleteConfirmationOutcome::Deleted {
        provider_name,
        needs_reselection,
    } = outcome
    else {
        return Ok(());
    };
    let mut body = vec![format!("Removed provider \"{provider_name}\".")];
    if *needs_reselection {
        body.push("Select a provider to make it active.".to_string());
    }
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: "Provider deleted",
            body,
            footer: None,
        },
    )
}
