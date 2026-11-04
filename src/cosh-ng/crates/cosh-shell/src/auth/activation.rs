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
        if error.provider_updated {
            if existing.provider_type == "aliyun"
                && existing.auth_source.as_deref() == Some("ecs_identity")
            {
                state.auth.ecs_trial.auto_use_allowed = true;
                state.auth.ecs_trial.disable_save_failed = false;
            }
            return super::completion::finish_partial_configuration(state, output);
        }
        if let Some(auth) = state.auth.state.as_mut() {
            auth.field_capture_revision = auth.field_capture_revision.wrapping_add(1);
        }
        clear_active_auth_panel(state, output)?;
        renderer.write_notice_panel(
            output,
            NoticePanelModel {
                title: "Provider switch not confirmed",
                body: vec![
                    error.message,
                    "Review the error and retry when ready.".into(),
                ],
                footer: None,
            },
        )?;
        render_current_auth_panel(state, output)?;
        return output.flush();
    }
    if existing.provider_type == "aliyun" && existing.auth_source.as_deref() == Some("ecs_identity")
    {
        state.auth.ecs_trial.auto_use_allowed = true;
        state.auth.ecs_trial.disable_save_failed = false;
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

#[cfg(test)]
mod access_copy_tests {
    use super::*;
    #[test]
    fn access_copy_saved_provider_partial_activation_preserves_source_and_opt_out_semantics() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("registry.sh");
        std::fs::write(&script, r#"#!/bin/sh
read -r request
printf '%s\n' '{"success":false,"data":{"provider_updated":true,"error_code":"io_error"},"error":"session save failed"}'
"#).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        for source in ["ecs_identity", "ecs_ram_role", "static"] {
            let adapter = AdapterInstance::CoshCore(crate::adapter::CoshCoreAdapter::new(
                script.to_string_lossy(),
                false,
            ));
            let existing = ExistingProvider {
                name: "saved".into(),
                provider_type: "aliyun".into(),
                label: "Aliyun".into(),
                model: "model".into(),
                is_active: false,
                editable: true,
                source: "user".into(),
                base_url: None,
                api_key_mask: None,
                access_key_id_mask: None,
                access_key_secret_mask: None,
                security_token_mask: None,
                auth_source: Some(source.into()),
            };
            let mut state = InlineState::default();
            state.auth.ecs_trial.auto_use_allowed = false;
            state.auth.ecs_trial.disable_save_failed = true;
            state.auth.ecs_trial.cancelled_question = true;
            state.personalization.foreground_model = Some("previous".into());
            let mut output = Vec::new();
            activate_provider(&adapter, &mut state, &existing, &mut output).unwrap();
            assert_eq!(
                state.auth.ecs_trial.auto_use_allowed,
                source == "ecs_identity"
            );
            assert_eq!(
                state.auth.ecs_trial.disable_save_failed,
                source != "ecs_identity"
            );
            assert!(state.auth.ecs_trial.cancelled_question);
            assert!(state.personalization.foreground_model.is_none());
            let text = String::from_utf8(output).unwrap();
            assert!(text.contains("session could not be saved"), "{text}");
            assert!(!text.contains("Provider switch not confirmed"), "{text}");
        }
    }
}
