//! Destructive provider-delete confirmation and result rendering.

use std::io::{self, Write};

use crate::adapter::AdapterInstance;
use crate::runtime::prelude::{
    NoticePanelModel, QuestionInputFeedback, QuestionPanelModel, QuestionPanelPresentation,
    QuestionSelectionMode, RatatuiInlineRenderer,
};

use super::provider_management::{core_auth_delete, load_core_auth_state, AuthConfigureFailure};
use super::runtime::{AuthPhase, RuntimeAuthState};
use crate::runtime::guide::result::{ActionKind, ChangeFact, Verification};
use crate::runtime::state::InlineState;

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
        refresh_error: Option<String>,
    },
}

pub(super) fn submit_delete_confirmation(
    adapter: &AdapterInstance,
    auth: &mut RuntimeAuthState,
    provider_idx: usize,
    auto_use_allowed: bool,
) -> Result<DeleteConfirmationOutcome, AuthConfigureFailure> {
    if auth.selected_provider != DELETE_OPTION_INDEX {
        auth.phase = AuthPhase::ProviderAction { provider_idx };
        auth.selected_provider = 0;
        return Ok(DeleteConfirmationOutcome::Cancelled);
    }

    let existing = auth
        .existing_providers
        .get(provider_idx)
        .cloned()
        .ok_or_else(|| AuthConfigureFailure {
            message: "provider selection is no longer valid".into(),
            code: None,
            provider_updated: false,
            unconfirmed: false,
        })?;
    core_auth_delete(adapter, &existing.name)?;
    let AdapterInstance::CoshCore(cosh_core) = adapter else {
        unreachable!("core_auth_delete rejects non-core adapters");
    };
    let core_state = match load_core_auth_state(cosh_core) {
        Ok(state) => state,
        Err(error) => {
            auth.existing_providers.retain(|p| p.name != existing.name);
            auth.sysom.sync(&mut auth.existing_providers);
            auth.phase = AuthPhase::ManagingProviders;
            return Ok(DeleteConfirmationOutcome::Deleted {
                provider_name: existing.name,
                needs_reselection: false,
                refresh_error: Some(error),
            });
        }
    };
    if let Some(trial) = core_state.ecs_trial.as_ref() {
        let auto_use = core_state.trial_auto_use.unwrap_or(true) && auto_use_allowed;
        auth.sysom.refresh_trial(trial, auto_use);
    }
    auth.existing_providers = core_state.existing_providers;
    // Re-derive the SysOM slot from the reloaded list: deleting the promoted RAM-role
    // provider must bring the shortcut back rather than relabel its replacement.
    auth.sysom.sync(&mut auth.existing_providers);
    auth.phase = AuthPhase::ManagingProviders;
    auth.selected_provider = 0;
    let needs_reselection = existing.is_active
        && !auth.sysom.trial_active()
        && !auth
            .existing_providers
            .iter()
            .any(|provider| provider.is_active);

    Ok(DeleteConfirmationOutcome::Deleted {
        provider_name: existing.name,
        needs_reselection,
        refresh_error: None,
    })
}

pub(super) fn render_delete_confirmation<W: Write>(
    auth: &RuntimeAuthState,
    provider_idx: usize,
    panel_id: &str,
    renderer: &RatatuiInlineRenderer,
    presentation: QuestionPanelPresentation<'static>,
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
    renderer.write_question_panel_with_presentation(
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
        presentation,
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
        refresh_error,
    } = outcome
    else {
        return Ok(());
    };
    let mut body = vec![format!("Removed provider \"{provider_name}\".")];
    if let Some(error) = refresh_error {
        body.push(format!(
            "Provider deleted, but the service list could not be refreshed: {error}"
        ));
    }
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

pub(super) fn apply_delete_outcome(
    state: &mut InlineState,
    deleted_was_active: bool,
    outcome: &DeleteConfirmationOutcome,
) {
    if let DeleteConfirmationOutcome::Deleted { refresh_error, .. } = outcome {
        state.auth.record_fact(
            ActionKind::Delete,
            ChangeFact::Committed,
            Verification::Verified,
        );
        if let Some(error) = refresh_error {
            state.auth.outcome.error =
                Some(format!("Service deleted; list refresh failed: {error}"));
        }
        if state.auth.host.is_guide() {
            if let Some(auth) = state.auth.state.as_mut() {
                auth.selected_provider = super::menu::hosted_entry_index(
                    state.auth.host,
                    &auth.sysom,
                    auth.existing_providers.len(),
                    super::menu::AuthManagementEntry::GuideExit,
                );
            }
        }
    }
    super::runtime::clear_observed_model_after_provider_delete(state, deleted_was_active, outcome);
}

pub(super) fn handle_delete_confirmation<W: Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    provider_idx: usize,
    output: &mut W,
) -> io::Result<bool> {
    let Some(auth) = state.auth.state.as_mut() else {
        return Ok(false);
    };
    let active = auth
        .existing_providers
        .get(provider_idx)
        .is_some_and(|p| p.is_active);
    let result = submit_delete_confirmation(
        adapter,
        auth,
        provider_idx,
        state.auth.ecs_trial.auto_use_allowed,
    );
    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            super::standalone::record_write_result(state, ActionKind::Delete, &Err(error.clone()));
            super::prompt::clear_active_auth_panel(state, output)?;
            renderer.write_notice_panel(
                output,
                NoticePanelModel {
                    title: if error.unconfirmed {
                        "Change unconfirmed"
                    } else {
                        "Provider was not deleted"
                    },
                    body: vec![error.message],
                    footer: None,
                },
            )?;
            super::prompt::render_current_auth_panel(state, output)?;
            return Ok(true);
        }
    };
    if matches!(outcome, DeleteConfirmationOutcome::Cancelled) && state.auth.host.is_guide() {
        if let Some(auth) = state.auth.state.as_mut() {
            super::navigation::return_from_delete(auth, provider_idx);
        }
    }
    apply_delete_outcome(state, active, &outcome);
    super::prompt::clear_active_auth_panel(state, output)?;
    render_delete_outcome(&outcome, &renderer, output)?;
    super::prompt::render_current_auth_panel(state, output)?;
    Ok(true)
}
