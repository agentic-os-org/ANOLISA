//! Backwards navigation for the multi-step `/auth` flow.
//!
//! `/auth` asks for one field per panel, so ESC has to mean "back one step" before it means
//! "abandon the flow": a typo in the Model field must not cost the user the API key and Base URL
//! they already confirmed. The decision is a pure transition on [`RuntimeAuthState`] so the card
//! dispatcher only has to choose between re-rendering the panel and cancelling it.

use super::menu::{management_entry_index, AuthManagementEntry};
use super::provider_management::{provider_actions, ExistingProvider, ProviderAction};
use super::runtime::{AuthPhase, RuntimeAuthState};

/// What the caller must do after ESC was applied to the pending auth panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BackOutcome {
    /// The flow moved one step back; clear the stale panel and render the current one.
    Redraw,
    /// There is no earlier step to return to; cancel the whole flow.
    Cancel,
}

/// Moves the flow one step back, reporting whether there was a step left to take.
pub(super) fn step_back(auth: &mut RuntimeAuthState) -> BackOutcome {
    // Every other phase is a menu the user reaches in one keystroke, so ESC there keeps the
    // cancel it already had.
    if auth.phase != AuthPhase::FillingField {
        return BackOutcome::Cancel;
    }
    if let Some(previous) = (0..auth.current_field.min(auth.current_provider().fields.len()))
        .rev()
        .find(|&index| auth.field_is_editable(index))
    {
        auth.current_field = previous;
        auth.field_error = None;
        // `collected_values` is the form and `field_input` only the editable projection of the
        // field under the cursor, so re-projecting is what both restores the earlier value and
        // discards the draft — including a secret one — typed into the field being left.
        auth.load_current_field_input();
        return BackOutcome::Redraw;
    }
    leave_form(auth)
}

/// Returns from the first field to the menu the form was entered from.
///
/// A `Cancel` outcome leaves the state untouched, so the caller's cancellation sees exactly the
/// flow the user pressed ESC on.
fn leave_form(auth: &mut RuntimeAuthState) -> BackOutcome {
    let Some(provider_name) = auth.editing_provider_name.as_deref() else {
        if auth.from_sysom_shortcut {
            auth.phase = AuthPhase::ManagingProviders;
            auth.selected_provider = management_entry_index(
                &auth.sysom,
                auth.existing_providers.len(),
                AuthManagementEntry::SysomShortcut,
            );
        } else {
            auth.phase = AuthPhase::SelectingProvider;
        }
        discard_field_draft(auth);
        return BackOutcome::Redraw;
    };
    let Some(provider_idx) = auth
        .existing_providers
        .iter()
        .position(|existing| existing.name == provider_name)
    else {
        // Without the row the edit started from there is no action menu to return to; an
        // `AuthRequired` prompt raised by a running agent carries no saved-provider list.
        return BackOutcome::Cancel;
    };
    // `selected_provider` indexes the action list in this phase, so the row has to be resolved
    // rather than reset: it decides what the next Enter does.
    let selected = auth
        .existing_providers
        .get(provider_idx)
        .map_or(0, edit_action_row);
    auth.phase = AuthPhase::ProviderAction { provider_idx };
    auth.selected_provider = selected;
    discard_field_draft(auth);
    BackOutcome::Redraw
}

/// Drops what the abandoned field held, since no later phase re-projects `field_input`.
///
/// Inside the form `load_current_field_input` overwrites the draft, but on the way out to a menu
/// nothing does — and the draft can be a secret the user typed and never submitted, which must
/// not outlive the prompt that asked for it.
fn discard_field_draft(auth: &mut RuntimeAuthState) {
    auth.field_input.clear();
    auth.field_error = None;
}

/// Row of "Edit configuration" in this provider's action menu.
///
/// `ManagingProviders` opens that menu at row 0, but row 0 is "Set as active provider" for an
/// inactive provider — returning there from an edit would turn the next Enter into a provider
/// switch. The `0` fallback is unreachable for a provider whose edit form is open, because Edit
/// is offered exactly when `editable` is set.
fn edit_action_row(existing: &ExistingProvider) -> usize {
    provider_actions(existing.is_active, existing.editable, existing.deletable())
        .iter()
        .position(|action| *action == ProviderAction::Edit)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests;
