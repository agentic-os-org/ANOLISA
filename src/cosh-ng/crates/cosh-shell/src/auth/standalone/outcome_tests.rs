use super::*;
use crate::adapter::{adapter_for_kind, AdapterKind, RegistryQueryError};
use crate::auth::provider_management::{
    auth_configure_failure, AuthConfigureFailure, ExistingProvider,
};
use crate::auth::runtime::{self, AuthBackend, AuthPhase, RuntimeAuthState};
use crate::auth::{capture::auth_capture_id, menu::SysomMenu, trial};
use crate::runtime::guide::result::{ActionFacts, ActionKind};
use crate::runtime::prelude::AuthProviderInfo;
use std::sync::mpsc;

fn guide_state() -> InlineState {
    let mut state = InlineState {
        language: Language::EnUs,
        ..Default::default()
    };
    state.auth.host = AuthHost::Guide;
    state.auth.state = Some(RuntimeAuthState {
        id: "auth-outcome".into(),
        request_id: "outcome".into(),
        phase: AuthPhase::ManagingProviders,
        providers: vec![AuthProviderInfo {
            id: "aliyun".into(),
            label: "Aliyun".into(),
            description: None,
            description_zh_cn: None,
            builtin_base_url: None,
            fields: Vec::new(),
        }],
        selected_provider: 0,
        current_field: 0,
        collected_values: Default::default(),
        field_input: String::new(),
        field_error: None,
        field_capture_revision: 0,
        existing_providers: vec![ExistingProvider {
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
            auth_source: None,
        }],
        editing_provider_name: None,
        default_provider_id: false,
        from_sysom_shortcut: false,
        error_message: None,
        backend: AuthBackend::CoreRegistry,
        sysom: SysomMenu::default(),
    });
    state
}

fn transport_failure() -> AuthConfigureFailure {
    auth_configure_failure(RegistryQueryError::Transport("owner receipt lost".into()))
}

fn owner_failure(code: Option<&str>, partial: bool) -> AuthConfigureFailure {
    auth_configure_failure(RegistryQueryError::Response {
        message: code.unwrap_or("provider not found").into(),
        code: code.map(str::to_owned),
        provider_updated: partial,
    })
}

fn pending_preference(state: &mut InlineState, result: Result<(), AuthConfigureFailure>) {
    let (send, receive) = mpsc::channel();
    state.auth.ecs_trial.preference_job = Some(receive);
    send.send(result).unwrap();
}

fn poll_preference(state: &mut InlineState) {
    trial::poll(&adapter_for_kind(AdapterKind::Fake), state, &mut Vec::new()).unwrap();
    assert!(state.auth.ecs_trial.preference_job.is_none());
}

fn attempt_held_write(state: &mut InlineState, kind: ActionKind) {
    let fake = adapter_for_kind(AdapterKind::Fake);
    let mut output = Vec::new();
    match kind {
        ActionKind::Activate | ActionKind::Delete => {
            let auth = state.auth.state.as_mut().unwrap();
            auth.phase = if kind == ActionKind::Activate {
                AuthPhase::ProviderAction { provider_idx: 0 }
            } else {
                AuthPhase::ConfirmDelete { provider_idx: 0 }
            };
            auth.selected_provider = usize::from(kind == ActionKind::Delete);
            let id = auth_capture_id(auth);
            runtime::handle_auth_answer(&fake, state, &id, "", &mut output).unwrap();
        }
        ActionKind::Configure => {
            state.auth.state.as_mut().unwrap().phase = AuthPhase::FillingField;
            runtime::send_auth_response(Some(&fake), state, &mut output).unwrap();
        }
        ActionKind::TrialPreference => {
            trial::set_preference_from_auth(&fake, state, &mut output, true).unwrap();
        }
        ActionKind::LoginShell => unreachable!(),
    }
    let text = String::from_utf8(output).unwrap();
    assert!(text.contains("Change unconfirmed"), "{kind:?}: {text}");
    assert!(state.auth.ecs.is_none());
    assert!(state.auth.ecs_trial.preference_job.is_none());
}

#[test]
fn rereview_fix_typed_unknown_blocks_each_write_now_and_after_management_reopens() {
    for kind in [
        ActionKind::Activate,
        ActionKind::Delete,
        ActionKind::Configure,
        ActionKind::TrialPreference,
    ] {
        let mut state = guide_state();
        record_write_result(&mut state, kind, &Err(transport_failure()));
        assert!(state.auth.outcome.facts(kind).unknown);
        attempt_held_write(&mut state, kind);
        attempt_held_write(&mut state, kind);
        state.auth.exit_requested = true;
        converge(&mut state, &mut Vec::new());
        let mut reopened = guide_state();
        reopened.auth.blocked_writes = state.auth.outcome.unknown_kinds().into_iter().collect();
        attempt_held_write(&mut reopened, kind);
        record_write_result(&mut reopened, kind, &Ok(()));
        assert!(
            reopened.auth.write_blocked(kind),
            "later reads/success cannot erase Unknown"
        );
    }
}

#[test]
fn rereview_fix_activation_typed_rejection_without_code_remains_retryable() {
    let mut state = guide_state();
    record_write_result(
        &mut state,
        ActionKind::Activate,
        &Err(owner_failure(None, false)),
    );
    assert_eq!(
        state.auth.outcome.facts(ActionKind::Activate),
        ActionFacts::default()
    );
    assert_eq!(
        state.auth.outcome.failure(ActionKind::Activate),
        Some("provider not found")
    );
    assert!(!state.auth.write_blocked(ActionKind::Activate));
    assert!(!refuse_unconfirmed_write(&mut state, ActionKind::Activate, &mut Vec::new()).unwrap());
    record_write_result(&mut state, ActionKind::Activate, &Ok(()));
    assert!(state.auth.outcome.facts(ActionKind::Activate).committed);
    assert!(!state.auth.outcome.unconfirmed_any());
}

fn converge_preference(result: Result<(), AuthConfigureFailure>, expected: ActionFacts) {
    let mut state = guide_state();
    pending_preference(&mut state, result.clone());
    state.auth.outcome.reason = EndReason::Cancelled;
    state.auth.exit_requested = true;
    converge(&mut state, &mut Vec::new());
    assert_eq!(
        state.auth.outcome.facts(ActionKind::TrialPreference),
        expected
    );
    assert_eq!(state.auth.outcome.reason, EndReason::Cancelled);
    assert_eq!(
        state.auth.write_blocked(ActionKind::TrialPreference),
        expected.unknown
    );
    assert!(state.auth.ecs_trial.preference_job.is_none());
    if let Err(error) = result {
        if !error.unconfirmed && !error.provider_updated {
            assert_eq!(
                state.auth.outcome.failure(ActionKind::TrialPreference),
                Some(error.message.as_str())
            );
        }
    }
}

#[test]
fn rereview_fix_preference_converge_retains_original_success() {
    converge_preference(
        Ok(()),
        ActionFacts {
            committed: true,
            ..Default::default()
        },
    );
}

#[test]
fn rereview_fix_preference_converge_retains_original_partial() {
    converge_preference(
        Err(owner_failure(Some("io_error"), true)),
        ActionFacts {
            partial: true,
            ..Default::default()
        },
    );
}

#[test]
fn rereview_fix_preference_converge_retains_explicit_failure() {
    converge_preference(
        Err(owner_failure(Some("EcsIdentityUnavailable"), false)),
        ActionFacts::default(),
    );
}

#[test]
fn rereview_fix_preference_converge_retains_lost_receipt_as_unknown() {
    let mut state = guide_state();
    let (send, receive) = mpsc::channel();
    state.auth.ecs_trial.preference_job = Some(receive);
    drop(send);
    state.auth.outcome.reason = EndReason::Cancelled;
    state.auth.exit_requested = true;
    converge(&mut state, &mut Vec::new());
    assert!(
        state
            .auth
            .outcome
            .facts(ActionKind::TrialPreference)
            .unknown
    );
    assert!(state.auth.write_blocked(ActionKind::TrialPreference));
    assert_eq!(state.auth.outcome.reason, EndReason::Cancelled);
}

#[test]
fn rereview_fix_preference_typed_rejection_preserves_error_and_allows_later_success() {
    let mut state = guide_state();
    state.auth.ecs_trial.enabling = true;
    state.auth.ecs_trial.auto_use_allowed = false;
    state.personalization.foreground_model = Some("own-model".into());
    pending_preference(
        &mut state,
        Err(owner_failure(Some("EcsIdentityUnavailable"), false)),
    );
    poll_preference(&mut state);
    assert_eq!(
        state.auth.outcome.facts(ActionKind::TrialPreference),
        ActionFacts::default()
    );
    assert_eq!(
        state.auth.outcome.failure(ActionKind::TrialPreference),
        Some("EcsIdentityUnavailable")
    );
    assert!(!state.auth.write_blocked(ActionKind::TrialPreference));
    assert!(!state.auth.ecs_trial.auto_use_allowed);
    assert_eq!(
        state.personalization.foreground_model.as_deref(),
        Some("own-model")
    );
    state.auth.ecs_trial.enabling = true;
    pending_preference(&mut state, Ok(()));
    poll_preference(&mut state);
    assert!(
        state
            .auth
            .outcome
            .facts(ActionKind::TrialPreference)
            .committed
    );
    assert!(state.auth.ecs_trial.auto_use_allowed);
    assert!(state.personalization.foreground_model.is_none());
}

#[test]
fn rereview_fix_preference_typed_owner_uncertainty_survives_poll_and_converge() {
    for code in [
        "trial_preferences_io",
        "trial_preferences_worker",
        "trial_activation_save_failed_preference_restore_failed",
    ] {
        for enabling in [false, true] {
            for cancelling in [false, true] {
                let mut state = guide_state();
                state.auth.ecs_trial.enabling = enabling;
                pending_preference(&mut state, Err(owner_failure(Some(code), false)));
                if cancelling {
                    state.auth.outcome.reason = EndReason::Cancelled;
                    state.auth.exit_requested = true;
                    converge(&mut state, &mut Vec::new());
                    state.auth.state = guide_state().auth.state;
                } else {
                    poll_preference(&mut state);
                }
                let facts = state.auth.outcome.facts(ActionKind::TrialPreference);
                assert!(
                    facts.unknown && !facts.committed && !facts.partial,
                    "{code}"
                );
                attempt_held_write(&mut state, ActionKind::TrialPreference);
                assert!(state.auth.outcome.unconfirmed_any());
            }
        }
    }
}

#[test]
fn rereview_fix_preference_typed_transport_failure_survives_poll_and_converge() {
    for cancelling in [false, true] {
        let mut state = guide_state();
        pending_preference(&mut state, Err(transport_failure()));
        if cancelling {
            state.auth.outcome.reason = EndReason::Cancelled;
            state.auth.exit_requested = true;
            converge(&mut state, &mut Vec::new());
            state.auth.state = guide_state().auth.state;
        } else {
            poll_preference(&mut state);
        }
        assert!(
            state
                .auth
                .outcome
                .facts(ActionKind::TrialPreference)
                .unknown
        );
        attempt_held_write(&mut state, ActionKind::TrialPreference);
    }
}

#[test]
fn publish_fix_preference_success_clears_only_its_earlier_failure_from_summary() {
    let mut state = guide_state();
    pending_preference(
        &mut state,
        Err(owner_failure(Some("EcsIdentityUnavailable"), false)),
    );
    poll_preference(&mut state);
    assert!(state
        .auth
        .outcome
        .message(state.i18n())
        .contains("EcsIdentityUnavailable"));
    pending_preference(&mut state, Ok(()));
    poll_preference(&mut state);
    assert_eq!(
        state.auth.outcome.failure(ActionKind::TrialPreference),
        None
    );
    assert!(!state
        .auth
        .outcome
        .message(state.i18n())
        .contains("EcsIdentityUnavailable"));
    assert!(
        state
            .auth
            .outcome
            .facts(ActionKind::TrialPreference)
            .committed
    );
    assert_eq!(state.auth.outcome.reason, EndReason::Done);
    assert!(state.auth.outcome.error.is_none());
}

#[test]
fn publish_fix_other_action_success_keeps_the_existing_failure_in_summary() {
    let mut state = guide_state();
    record_write_result(
        &mut state,
        ActionKind::Activate,
        &Err(owner_failure(None, false)),
    );
    pending_preference(&mut state, Ok(()));
    poll_preference(&mut state);
    assert_eq!(
        state.auth.outcome.failure(ActionKind::Activate),
        Some("provider not found")
    );
    assert_eq!(
        state.auth.outcome.failure(ActionKind::TrialPreference),
        None
    );
    assert!(state
        .auth
        .outcome
        .message(state.i18n())
        .contains("provider not found"));
    assert!(
        state
            .auth
            .outcome
            .facts(ActionKind::TrialPreference)
            .committed
    );
    assert!(state.auth.outcome.error.is_none());
}
