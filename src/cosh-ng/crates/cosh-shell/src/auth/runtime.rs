use std::collections::{HashMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::adapter::AdapterInstance;
use crate::auth::active_submission::finish_active_submission;
use crate::auth::capture::matches_auth_capture;
use crate::auth::completion::finish_auth_configuration;
use crate::auth::default_id::{
    begin_new_provider, begin_provider_fields, begin_sysom_shortcut,
    providers_with_provider_id_field,
};
use crate::auth::delete_confirm::{
    begin_delete_confirmation, focus_delete_confirmation, render_delete_outcome,
    submit_delete_confirmation, DeleteConfirmationOutcome,
};
use crate::auth::menu::{
    has_manageable_entries, management_entry, management_entry_count, management_entry_index,
    AuthManagementEntry, EcsRamRolePrepare, PrefetchedAliyunPrepare, SysomMenu,
};
use crate::auth::navigation::{step_back, BackOutcome};
use crate::auth::prompt::{clear_active_auth_panel, render_current_auth_panel};
use crate::auth::provider_display::auth_providers_for_display;
use crate::auth::provider_management::{
    core_auth_activate, core_auth_configure, load_core_auth_state, provider_action_choice,
    ExistingProvider, ProviderAction,
};
use crate::auth::retry::restore_after_failed_submission_at;
use crate::auth::validation::{record_field_edit, record_field_submission, FieldSubmission};
use crate::runtime::dispatcher::stable_event_key;
use crate::runtime::prelude::{
    AuthFieldInfo, AuthProviderInfo, AuthResponse, NoticePanelModel, RatatuiInlineRenderer,
    ShellEvent, ShellEventKind,
};
use crate::runtime::state::InlineState;

pub(crate) use crate::auth::capture::pending_auth_capture;
pub(crate) use crate::auth::required::{record_auth_required, render_auth_panel};

#[derive(Debug, Clone)]
pub(crate) struct RuntimeAuthState {
    pub(crate) id: String,
    #[allow(dead_code)]
    pub(crate) request_id: String,
    pub(crate) phase: AuthPhase,
    pub(crate) providers: Vec<AuthProviderInfo>,
    pub(crate) selected_provider: usize,
    pub(crate) current_field: usize,
    pub(crate) collected_values: HashMap<String, String>,
    pub(crate) field_input: String,
    /// Inline validation error for the field being filled; cleared on the next edit.
    pub(crate) field_error: Option<String>,
    /// Changes when inline validation re-arms the current field's input capture.
    pub(crate) field_capture_revision: u64,
    /// Existing providers loaded from config.toml (for ManagingProviders phase)
    pub(crate) existing_providers: Vec<ExistingProvider>,
    /// The section name of the provider being edited (None = new provider)
    pub(crate) editing_provider_name: Option<String>,
    pub(super) default_provider_id: bool,
    pub(super) from_sysom_shortcut: bool,
    pub(super) error_message: Option<String>,
    pub(super) backend: AuthBackend,
    /// SysOM placement plus the Aliyun prepare result prefetched for this `/auth`.
    /// Default (non-ECS) leaves every menu index and phase transition unchanged.
    pub(super) sysom: SysomMenu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AuthBackend {
    ActiveRun,
    CoreRegistry,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AuthPhase {
    PreparingMenu,
    /// Show existing providers list + "Add new" option
    ManagingProviders,
    /// Action menu after selecting an existing provider
    ProviderAction {
        provider_idx: usize,
    },
    /// Require explicit confirmation before removing a user-owned provider.
    ConfirmDelete {
        provider_idx: usize,
    },
    SelectingProvider,
    FillingField,
    AliyunEcsPreparing,
    AliyunEcsChallenge {
        instance_id: String,
        console_url: String,
    },
}

impl RuntimeAuthState {
    pub(super) fn current_provider(&self) -> &AuthProviderInfo {
        &self.providers[self.selected_provider]
    }

    pub(super) fn current_field_info(&self) -> Option<&AuthFieldInfo> {
        self.current_provider().fields.get(self.current_field)
    }

    fn all_fields_collected(&self) -> bool {
        self.current_field >= self.current_provider().fields.len()
    }

    /// Re-projects the field cursor onto the editable buffer.
    ///
    /// `collected_values` is the form's data; `field_input` is only the editable projection of
    /// the field the cursor sits on. Every move of `current_field` has to go through here, or
    /// the panel shows a value the form no longer holds.
    pub(super) fn load_current_field_input(&mut self) {
        let field_name = self.current_field_info().map(|field| field.name.clone());
        self.field_input = field_name
            .and_then(|name| self.collected_values.get(&name).cloned())
            .unwrap_or_default();
    }
}

#[derive(Debug, Default)]
pub(crate) struct AuthState {
    pub(crate) state: Option<RuntimeAuthState>,
    pub(super) ecs: Option<super::ecs_poll::EcsFlow>,
    pub(crate) handled_card_events: HashSet<String>,
    pub(crate) completed_ids: HashSet<String>,
}

pub(crate) fn has_pending_auth(state: &InlineState) -> bool {
    state.auth.state.is_some()
}

/// Trigger auth panel from `/auth` slash command.
/// Now starts in ManagingProviders phase to show existing providers.
pub(crate) fn trigger_auth_from_slash<W: std::io::Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.auth.state.is_some() {
        return Ok(());
    }
    let AdapterInstance::CoshCore(cosh_core) = adapter else {
        let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
        renderer.write_notice_panel(
            output,
            NoticePanelModel {
                title: "Auth unavailable",
                body: vec![
                    "Authentication is managed by cosh-core.".to_string(),
                    "Switch to the cosh-core backend before running /auth.".to_string(),
                ],
                footer: None,
            },
        )?;
        return Ok(());
    };

    let core_state = match load_core_auth_state(cosh_core) {
        Ok(state) => state,
        Err(message) => {
            let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
            renderer.write_notice_panel(
                output,
                NoticePanelModel {
                    title: "Auth unavailable",
                    body: vec![
                        "Unable to read auth state from cosh-core.".to_string(),
                        message,
                    ],
                    footer: None,
                },
            )?;
            return Ok(());
        }
    };

    let providers =
        auth_providers_for_display(&providers_with_provider_id_field(core_state.templates));
    let request_id = format!(
        "slash-auth-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
    );
    let id = format!("auth-{request_id}");

    state.auth.state = Some(RuntimeAuthState {
        id: id.clone(),
        request_id,
        phase: AuthPhase::PreparingMenu,
        providers,
        selected_provider: 0,
        current_field: 0,
        collected_values: HashMap::new(),
        field_input: String::new(),
        field_error: None,
        field_capture_revision: 0,
        existing_providers: core_state.existing_providers,
        editing_provider_name: None,
        default_provider_id: false,
        from_sysom_shortcut: false,
        error_message: None,
        backend: AuthBackend::CoreRegistry,
        sysom: SysomMenu::default(),
    });

    render_current_auth_panel(state, output)?;
    Ok(())
}

pub(super) fn clear_observed_model_after_provider_change(state: &mut InlineState) {
    state.personalization.foreground_model = None;
    // Credentials just changed: drop the startup probe verdict so the
    // no-auth surfaces re-resolve instead of trusting a pre-change
    // snapshot (a stale `Some(false)` would keep suppressing the
    // recommendation disclosure for the rest of the session).
    state.startup_auth.pending = None;
    state.startup_auth.resolved = None;
}

fn clear_observed_model_after_provider_delete(
    state: &mut InlineState,
    deleted_provider_was_active: bool,
    outcome: &DeleteConfirmationOutcome,
) {
    if deleted_provider_was_active && matches!(outcome, DeleteConfirmationOutcome::Deleted { .. }) {
        clear_observed_model_after_provider_change(state);
    }
}

#[derive(Debug, Deserialize)]
struct CoreAuthPrepare {
    mode: String,
    instance_id: Option<String>,
    console_url: Option<String>,
    #[serde(default)]
    values: HashMap<String, String>,
}

pub(super) fn finish_sysom_menu_prepare(
    auth: &mut RuntimeAuthState,
    result: Result<Value, String>,
) {
    let prepare = result.and_then(|value| {
        serde_json::from_value::<CoreAuthPrepare>(value)
            .map_err(|_| "Invalid auth menu prepare response".to_string())
    });
    auth.sysom = match prepare {
        Ok(prepare) if prepare.mode == "manual" => SysomMenu::on_manual(),
        Ok(prepare) => ecs_ram_role_prepare(prepare)
            .map(SysomMenu::on_ecs)
            .unwrap_or_default(),
        Err(error) => {
            // A failed recommendation must not prevent managing existing providers.
            tracing::debug!("auth prepare for the SysOM menu entry failed: {error}");
            SysomMenu::default()
        }
    };
    auth.sysom.sync(&mut auth.existing_providers);
    auth.phase = if has_manageable_entries(&auth.sysom, auth.existing_providers.len()) {
        AuthPhase::ManagingProviders
    } else {
        AuthPhase::SelectingProvider
    };
}

fn ecs_ram_role_prepare(prepare: CoreAuthPrepare) -> Option<EcsRamRolePrepare> {
    (prepare.mode == "ecs_ram_role").then(|| EcsRamRolePrepare {
        instance_id: prepare.instance_id.unwrap_or_default(),
        console_url: prepare.console_url.unwrap_or_default(),
        values: prepare.values,
    })
}

fn handle_auth_focus<W: std::io::Write>(
    state: &mut InlineState,
    id: &str,
    selected: usize,
    output: &mut W,
) -> std::io::Result<bool> {
    let Some(auth) = state.auth.state.as_mut() else {
        return Ok(false);
    };
    if !matches_auth_capture(auth, id) {
        return Ok(false);
    }
    match auth.phase {
        AuthPhase::ManagingProviders => {
            let entries = management_entry_count(&auth.sysom, auth.existing_providers.len());
            auth.selected_provider = selected.min(entries.saturating_sub(1));
            clear_active_auth_panel(state, output)?;
            render_current_auth_panel(state, output)?;
        }
        AuthPhase::ProviderAction { .. } => {
            auth.selected_provider = selected;
            clear_active_auth_panel(state, output)?;
            render_current_auth_panel(state, output)?;
        }
        AuthPhase::ConfirmDelete { .. } => {
            focus_delete_confirmation(auth, selected);
            clear_active_auth_panel(state, output)?;
            render_current_auth_panel(state, output)?;
        }
        AuthPhase::SelectingProvider => {
            auth.selected_provider = selected.min(auth.providers.len().saturating_sub(1));
            clear_active_auth_panel(state, output)?;
            render_current_auth_panel(state, output)?;
        }
        _ => {}
    }
    Ok(true)
}

fn handle_auth_input<W: std::io::Write>(
    state: &mut InlineState,
    id: &str,
    text: &str,
    output: &mut W,
) -> std::io::Result<bool> {
    let Some(auth) = state.auth.state.as_mut() else {
        return Ok(false);
    };
    if !matches_auth_capture(auth, id) {
        return Ok(false);
    }
    if auth.phase == AuthPhase::FillingField {
        record_field_edit(auth, text);
        clear_active_auth_panel(state, output)?;
        render_current_auth_panel(state, output)?;
    }
    Ok(true)
}

fn handle_auth_answer<W: std::io::Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    id: &str,
    raw_answer: &str,
    output: &mut W,
) -> std::io::Result<bool> {
    let Some(auth) = state.auth.state.as_mut() else {
        return Ok(false);
    };
    if !matches_auth_capture(auth, id) {
        return Ok(false);
    }

    match auth.phase {
        AuthPhase::ManagingProviders => {
            let entry = management_entry(
                &auth.sysom,
                auth.existing_providers.len(),
                auth.selected_provider,
            );
            match entry {
                AuthManagementEntry::Existing(provider_idx) => {
                    // Selected an existing provider -> show action menu
                    auth.phase = AuthPhase::ProviderAction { provider_idx };
                    auth.selected_provider = 0;
                }
                // The SysOM shortcut is the aliyun template with the ECS challenge already
                // in hand; a template list without `aliyun` falls back to the picker.
                AuthManagementEntry::SysomShortcut => {
                    if !begin_sysom_shortcut(auth) {
                        begin_new_provider(auth);
                    } else if auth.default_provider_id {
                        apply_aliyun_prepare(auth);
                    }
                }
                AuthManagementEntry::AddNew => begin_new_provider(auth),
            }
            render_or_submit_auth(adapter, state, output)?;
            Ok(true)
        }
        AuthPhase::ProviderAction { provider_idx } => {
            let existing = auth.existing_providers[provider_idx].clone();
            let is_active = existing.is_active;
            let editable = existing.editable;
            let deletable = existing.deletable();

            let action =
                provider_action_choice(is_active, editable, deletable, auth.selected_provider);

            match action {
                ProviderAction::Activate => {
                    core_auth_activate(adapter, &existing.name).map_err(std::io::Error::other)?;
                    clear_observed_model_after_provider_change(state);
                    // Clear and show confirmation
                    state.auth.state.take();
                    clear_active_auth_panel(state, output)?;
                    let renderer =
                        RatatuiInlineRenderer::for_terminal().with_language(state.language);
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
                    output.flush()?;
                }
                ProviderAction::Edit => {
                    // Enter edit mode for this provider
                    let provider_type = existing.provider_type.as_str();
                    let template_idx = auth
                        .providers
                        .iter()
                        .position(|provider| provider.id == provider_type)
                        .or_else(|| {
                            auth.providers
                                .iter()
                                .position(|provider| provider.id == "openai_compat")
                        })
                        .unwrap_or(0);

                    auth.selected_provider = template_idx;
                    auth.editing_provider_name = Some(existing.name.clone());
                    auth.default_provider_id = false;
                    auth.from_sysom_shortcut = false;

                    auth.collected_values
                        .insert("provider_id".to_string(), existing.name.clone());
                    if let Some(ref v) = existing.api_key_mask {
                        auth.collected_values
                            .insert("api_key".to_string(), v.clone());
                    }
                    if let Some(ref v) = existing.base_url {
                        auth.collected_values
                            .insert("base_url".to_string(), v.clone());
                    }
                    if !existing.model.is_empty() {
                        auth.collected_values
                            .insert("model".to_string(), existing.model.clone());
                    }
                    if let Some(ref v) = existing.access_key_id_mask {
                        auth.collected_values
                            .insert("access_key_id".to_string(), v.clone());
                    }
                    if let Some(ref v) = existing.access_key_secret_mask {
                        auth.collected_values
                            .insert("access_key_secret".to_string(), v.clone());
                    }
                    if let Some(ref v) = existing.security_token_mask {
                        auth.collected_values
                            .insert("security_token".to_string(), v.clone());
                    }
                    if let Some(ref v) = existing.auth_source {
                        auth.collected_values
                            .insert("auth_source".to_string(), v.clone());
                    }

                    if should_apply_aliyun_prepare_for_edit(&existing) {
                        if apply_aliyun_prepare(auth) {
                            clear_active_auth_panel(state, output)?;
                            render_current_auth_panel(state, output)?;
                            return Ok(true);
                        }
                        clear_ecs_auth_source_for_manual_aliyun_edit(
                            &existing,
                            &mut auth.collected_values,
                        );
                    }

                    auth.phase = AuthPhase::FillingField;
                    auth.current_field = auth.first_editable_field();
                    auth.load_current_field_input();
                    render_or_submit_auth(adapter, state, output)?;
                }
                ProviderAction::Delete => {
                    begin_delete_confirmation(auth, provider_idx);
                    clear_active_auth_panel(state, output)?;
                    render_current_auth_panel(state, output)?;
                }
                ProviderAction::Cancel => {
                    // Cancel -> back to ManagingProviders, on the same row we came from
                    auth.phase = AuthPhase::ManagingProviders;
                    auth.selected_provider = management_entry_index(
                        &auth.sysom,
                        auth.existing_providers.len(),
                        AuthManagementEntry::Existing(provider_idx),
                    );
                    clear_active_auth_panel(state, output)?;
                    render_current_auth_panel(state, output)?;
                }
            }
            Ok(true)
        }
        AuthPhase::ConfirmDelete { provider_idx } => {
            let deleted_provider_was_active = auth
                .existing_providers
                .get(provider_idx)
                .is_some_and(|provider| provider.is_active);
            let outcome = submit_delete_confirmation(adapter, auth, provider_idx)
                .map_err(std::io::Error::other)?;
            clear_observed_model_after_provider_delete(
                state,
                deleted_provider_was_active,
                &outcome,
            );
            clear_active_auth_panel(state, output)?;
            let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
            render_delete_outcome(&outcome, &renderer, output)?;
            render_current_auth_panel(state, output)?;
            Ok(true)
        }
        AuthPhase::SelectingProvider => {
            begin_provider_fields(auth);
            if auth.current_provider().id == "aliyun"
                && (should_apply_aliyun_prepare_on_provider_selection(auth.backend)
                    || auth.default_provider_id)
            {
                apply_aliyun_prepare(auth);
            }
            render_or_submit_auth(adapter, state, output)?;
            Ok(true)
        }
        AuthPhase::FillingField => {
            let value = if raw_answer.is_empty() {
                auth.field_input.clone()
            } else {
                raw_answer.to_string()
            };
            let field = auth.current_field_info().cloned();
            // Reject invalid input before any downstream work so the other fields survive.
            if record_field_submission(auth, field.as_ref(), value) == FieldSubmission::Rejected {
                clear_active_auth_panel(state, output)?;
                render_current_auth_panel(state, output)?;
                return Ok(true);
            }
            if should_apply_aliyun_prepare_after_field(
                auth.backend,
                auth.editing_provider_name.is_some(),
                auth.current_provider().id.as_str(),
                field.as_ref().map(|f| f.name.as_str()),
            ) && apply_aliyun_prepare(auth)
            {
                clear_active_auth_panel(state, output)?;
                render_current_auth_panel(state, output)?;
                return Ok(true);
            }
            auth.current_field = auth.editable_field_at_or_after(auth.current_field + 1);
            auth.load_current_field_input();
            render_or_submit_auth(adapter, state, output)?;
            Ok(true)
        }
        AuthPhase::PreparingMenu
        | AuthPhase::AliyunEcsPreparing
        | AuthPhase::AliyunEcsChallenge { .. } => {
            super::ecs_poll::answer(adapter, state, output)?;
            Ok(true)
        }
    }
}

fn render_or_submit_auth<W: std::io::Write>(
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    clear_active_auth_panel(state, output)?;
    if state
        .auth
        .state
        .as_ref()
        .is_some_and(|auth| auth.phase == AuthPhase::FillingField && auth.all_fields_collected())
    {
        send_auth_response(Some(adapter), state, output)
    } else {
        render_current_auth_panel(state, output)
    }
}

fn should_apply_aliyun_prepare_on_provider_selection(backend: AuthBackend) -> bool {
    backend == AuthBackend::ActiveRun
}

fn should_apply_aliyun_prepare_after_field(
    backend: AuthBackend,
    is_editing: bool,
    provider_type: &str,
    field_name: Option<&str>,
) -> bool {
    backend == AuthBackend::CoreRegistry
        && !is_editing
        && provider_type == "aliyun"
        && field_name == Some("provider_id")
}

fn should_apply_aliyun_prepare_for_edit(existing: &ExistingProvider) -> bool {
    existing.provider_type == "aliyun" && existing.auth_source.as_deref() == Some("ecs_ram_role")
}

fn clear_ecs_auth_source_for_manual_aliyun_edit(
    existing: &ExistingProvider,
    values: &mut HashMap<String, String>,
) {
    if should_apply_aliyun_prepare_for_edit(existing) {
        values.remove("auth_source");
    }
}

/// Switches the flow to the ECS RAM-role challenge, or reports `false` for manual AK/SK.
///
/// Reuses the challenge `/auth` already prefetched when there is one, so selecting the
/// SysOM shortcut does not probe the ECS metadata service a second time.
fn apply_aliyun_prepare(auth: &mut RuntimeAuthState) -> bool {
    match auth.sysom.prefetched() {
        Some(PrefetchedAliyunPrepare::Manual) => return false,
        Some(PrefetchedAliyunPrepare::EcsRamRole(prepare)) => {
            super::ecs_poll::set_challenge(auth, prepare.clone());
        }
        None => auth.phase = AuthPhase::AliyunEcsPreparing,
    }
    true
}

pub(super) fn send_auth_response<W: std::io::Write>(
    adapter: Option<&AdapterInstance>,
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if state.agent_run.active.is_none()
        && state.auth.state.as_ref().is_some_and(|auth| {
            auth.backend == AuthBackend::CoreRegistry
                && auth.collected_values.get("auth_source").map(String::as_str)
                    == Some("ecs_ram_role")
        })
    {
        let adapter = adapter.ok_or_else(|| std::io::Error::other("missing ECS auth adapter"))?;
        return super::ecs_poll::start_configuration(adapter, state, output);
    }
    let mut auth = state.auth.state.take().expect("auth state present");
    let provider = &auth.providers[auth.selected_provider];
    let provider_label = provider.label.clone();
    let provider_template_id = provider.id.clone();
    let provider_id = auth
        .editing_provider_name
        .clone()
        .or_else(|| auth.collected_values.get("provider_id").cloned())
        .unwrap_or_else(|| provider.id.clone());
    let response = AuthResponse {
        request_id: auth.request_id.clone(),
        provider_id: provider_id.clone(),
        provider_type: Some(provider.id.clone()),
        values: auth.collected_values.clone(),
        persist: true,
    };

    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    let validation_body = auth_validation_body(
        &provider.id,
        response.values.get("auth_source").map(String::as_str),
    );
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: "Validating configuration...",
            body: validation_body,
            footer: None,
        },
    )?;
    output.flush()?;

    if let Some(active_run) = state.agent_run.active.as_ref() {
        let result = active_run.handle.respond_auth(response);
        if result.is_ok() {
            clear_observed_model_after_provider_change(state);
        }
        return finish_active_submission(
            result,
            &auth.id,
            &mut state.auth.completed_ids,
            state.language,
            output,
        );
    } else {
        match auth.backend {
            AuthBackend::CoreRegistry => {
                let adapter = adapter.ok_or_else(|| {
                    std::io::Error::other("missing adapter for cosh-core auth registry")
                })?;
                if let Err(error) = core_auth_configure(adapter, &response) {
                    restore_after_configure_failure(
                        &mut auth,
                        error.code.as_deref(),
                        error.focus_field(&provider_template_id),
                    );
                    state.auth.state = Some(auth);
                    renderer.write_notice_panel(
                        output,
                        NoticePanelModel {
                            title: "Credentials were not saved",
                            body: vec![
                                error.message,
                                "Review the highlighted value and try again.".to_string(),
                            ],
                            footer: None,
                        },
                    )?;
                    render_current_auth_panel(state, output)?;
                    return Ok(());
                }
                clear_observed_model_after_provider_change(state);
            }
            AuthBackend::ActiveRun => {}
        }
    }

    state.auth.completed_ids.insert(auth.id);
    finish_auth_configuration(state, output, &provider_label)
}

fn restore_after_configure_failure(
    auth: &mut RuntimeAuthState,
    error_code: Option<&str>,
    field_name: Option<&str>,
) {
    if error_code == Some("credential_source_unavailable")
        && matches!(auth.phase, AuthPhase::AliyunEcsChallenge { .. })
    {
        auth.field_error = None;
        return;
    }
    restore_after_failed_submission_at(auth, field_name);
}

pub(super) fn auth_validation_body(provider_id: &str, auth_source: Option<&str>) -> Vec<String> {
    let mut body = vec!["Checking endpoint, credentials, and model.".to_string()];
    if matches!(
        provider_id,
        "dashscope" | "openai_compat" | "coding_plan" | "token_plan"
    ) || (provider_id == "aliyun" && auth_source != Some("ecs_ram_role"))
    {
        body.push(
            "If read-only model validation is unavailable or cannot verify credentials, validation may make a minimal chat request (max_tokens=1) that consumes a very small amount of quota or incurs a very small charge."
                .to_string(),
        );
    }
    body
}

/// Reports whether `event` carries the capture id the auth panel is currently listening on.
///
/// The scoped id is what keeps a keystroke left over from an earlier field from acting on
/// whichever field is live now.
fn event_targets_pending_auth(state: &InlineState, event: &ShellEvent) -> bool {
    let Some(target_id) = event.input.as_deref() else {
        return false;
    };
    state
        .auth
        .state
        .as_ref()
        .is_some_and(|auth| matches_auth_capture(auth, target_id.trim()))
}

/// Applies ESC to the pending auth panel: one step back, or cancel at the first step.
fn handle_auth_back<W: std::io::Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    let Some(auth) = state.auth.state.as_mut() else {
        return Ok(());
    };
    match step_back(auth) {
        BackOutcome::Redraw => {
            clear_active_auth_panel(state, output)?;
            render_current_auth_panel(state, output)
        }
        BackOutcome::Cancel => cancel_auth_panel(state, output),
    }
}

pub(super) fn cancel_auth_panel<W: std::io::Write>(
    state: &mut InlineState,
    output: &mut W,
) -> std::io::Result<()> {
    if super::ecs_poll::cancel(state, output)? {
        return Ok(());
    }
    clear_active_auth_panel(state, output)?;
    if let Some(auth) = state.auth.state.as_ref() {
        state.auth.completed_ids.insert(auth.id.clone());
    }
    state.auth.state = None;

    let renderer = RatatuiInlineRenderer::for_terminal().with_language(state.language);
    renderer.write_notice_panel(
        output,
        NoticePanelModel {
            title: "Auth cancelled",
            body: vec!["Authentication skipped.".to_string()],
            footer: None,
        },
    )?;

    crate::slash::prompt::write_shell_prompt(state, output)?;
    output.flush()?;
    Ok(())
}

pub(crate) fn render_auth_card_actions<W: std::io::Write>(
    events: &[ShellEvent],
    adapter: &AdapterInstance,
    state: &mut InlineState,
    output: &mut W,
    event_index_base: usize,
) -> std::io::Result<()> {
    if !has_pending_auth(state) {
        return Ok(());
    }
    for (idx, event) in events.iter().enumerate() {
        let event_index = event_index_base + idx;
        if event.kind != ShellEventKind::UserInputIntercepted {
            continue;
        }
        if !is_auth_card_component(event.component.as_deref()) {
            continue;
        }
        let dedup_key = stable_event_key("auth-card", event_index, event);
        if !state.auth.handled_card_events.insert(dedup_key) {
            continue;
        }
        match event.message.as_deref() {
            Some("focus") => {
                if let Some((id, selected)) = parse_card_id_usize(event) {
                    handle_auth_focus(state, &id, selected, output)?;
                }
            }
            Some("input") => {
                if let Some((id, text)) = parse_card_id_text(event) {
                    handle_auth_input(state, &id, &text, output)?;
                }
            }
            Some("answer") => {
                if let Some(answer) = event.input.as_deref() {
                    let auth_id = state.auth.state.as_ref().map(|a| a.id.clone());
                    if let Some(id) = auth_id {
                        handle_auth_answer(adapter, state, &id, answer, output)?;
                        let key = stable_event_key("question-answer", event_index, event);
                        state.questions.handled_answers.insert(key);
                    }
                }
            }
            // An empty Enter on a non-secret field arrives as `question_submit_empty`, not
            // `answer` (secret fields send an empty `answer` instead). Without this arm the
            // keystroke is dropped, so "Enter to keep current value" never advances an edit.
            // The event carries the scoped capture id, and routing it through the same
            // matches_auth_capture check is what keeps a stale `field-N` submission from
            // advancing whichever field is live now.
            Some("question_submit_empty") => {
                if let Some(capture_id) = event.input.as_deref() {
                    if handle_auth_answer(adapter, state, capture_id.trim(), "", output)? {
                        let key = stable_event_key("question-answer", event_index, event);
                        state.questions.handled_answers.insert(key);
                    }
                }
            }
            // ESC steps back through the form one prompt at a time; Ctrl+C
            // (`question_abort`) keeps abandoning `/auth` outright, so the multi-step flow
            // never costs the user their usual interrupt.
            Some("question_cancel") if event_targets_pending_auth(state, event) => {
                handle_auth_back(state, output)?;
            }
            Some("cancel") | Some("question_abort") if event_targets_pending_auth(state, event) => {
                cancel_auth_panel(state, output)?;
            }
            _ => {}
        }
    }
    Ok(())
}

fn is_auth_card_component(component: Option<&str>) -> bool {
    matches!(component, Some("card") | Some("card_secret"))
}

fn parse_card_id_usize(event: &ShellEvent) -> Option<(String, usize)> {
    let (id, val) = event.input.as_deref()?.split_once(':')?;
    let val = val.trim().parse::<usize>().ok()?;
    Some((id.trim().to_string(), val))
}

fn parse_card_id_text(event: &ShellEvent) -> Option<(String, String)> {
    let (id, text) = event.input.as_deref()?.split_once(':')?;
    Some((id.trim().to_string(), text.to_string()))
}

#[cfg(test)]
mod tests;
