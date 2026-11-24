use super::*;
use crate::auth::trial_policy::{self, ProviderIntent, TrialDecision};
use crate::config::ProviderConfig;

use trial_policy::MODEL;

fn saved_provider(config: &CoreConfig) -> Option<&str> {
    config
        .ai
        .active_provider
        .as_deref()
        .filter(|name| {
            config
                .ai
                .providers
                .get(*name)
                .is_some_and(ProviderConfig::is_ecs_identity)
        })
        .or_else(|| {
            config
                .ai
                .providers
                .iter()
                .filter_map(|(name, provider)| provider.is_ecs_identity().then_some(name.as_str()))
                .min()
        })
}

pub(super) async fn available() -> Result<bool, ProbeError> {
    #[cfg(test)]
    if let Ok(available) = super::TEST_ECS_PROBE.try_with(|(available, requests)| {
        requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *available
    }) {
        return available;
    }
    crate::provider::sysom::detect_ecs_auth_challenge()
        .await
        .map(|challenge| challenge.is_some())
        .inspect_err(|error| {
            tracing::warn!(
                stage = "identity",
                error_code = error.code(),
                "ECS identity availability probe failed"
            )
        })
}

pub(super) fn state(
    config: &CoreConfig,
    probe: Option<Result<bool, ProbeError>>,
    auto_use: bool,
) -> Value {
    let available = matches!(probe, Some(Ok(true)));
    let decision = trial_policy::decide(config, &ProviderIntent::from_env(), true, true, auto_use);
    let active = available && decision == TrialDecision::EcsIdentity;
    let route = match decision {
        TrialDecision::Existing => "existing",
        TrialDecision::EcsIdentity => "ecs_identity",
        TrialDecision::Disabled => "disabled",
    };
    let (identity_status, error_code) = match probe {
        Some(Ok(true)) => ("supported", None),
        Some(Err(error)) => ("failed", Some(error.code())),
        Some(Ok(false)) => ("unknown", None),
        None if cfg!(target_os = "macos") => ("unsupported", None),
        None => ("not_checked", None),
    };
    let provider_id = saved_provider(config);
    serde_json::json!({
        "available": available, "active": active, "model": MODEL, "provider_id": provider_id,
        "route": route, "identity_status": identity_status, "error_code": error_code,
    })
}

async fn save_preference(value: bool) -> Result<(), String> {
    tokio::task::spawn_blocking(move || crate::auth::trial_preferences::save(value))
        .await
        .map_err(|_| "trial_preferences_worker".to_string())?
        .map_err(|error| error.code().to_string())
}

fn error(request_id: &str, code: &str) -> OutputMessage {
    OutputMessage::RegistryResponse {
        request_id: request_id.into(),
        success: false,
        data: Some(serde_json::json!({"error_code":code})),
        error: Some(code.into()),
    }
}

pub(super) async fn activate(
    request_id: &str,
    config: &mut CoreConfig,
    selected: Option<&str>,
    guarded: bool,
) -> OutputMessage {
    match available().await {
        Ok(true) => {}
        Ok(false) => return error(request_id, "EcsIdentityUnavailable"),
        Err(failure) => return error(request_id, failure.code()),
    }
    let previous_preference =
        match tokio::task::spawn_blocking(crate::auth::trial_preferences::load).await {
            Ok(Ok(value)) => value,
            Ok(Err(failure)) => return error(request_id, failure.code()),
            Err(_) => return error(request_id, "trial_preferences_worker"),
        };
    let model = MODEL.to_string();
    if !previous_preference {
        if let Err(code) = save_preference(true).await {
            return error(request_id, &code);
        }
    }
    let persisted = crate::config::persist_auth_update(config, guarded, |candidate| {
        let provider_id = selected
            .or_else(|| saved_provider(candidate))
            .map(str::to_string)
            .unwrap_or_else(|| {
                let mut name = "ecs-free-trial".to_string();
                let mut suffix = 2;
                while candidate.ai.providers.contains_key(&name) {
                    name = format!("ecs-free-trial-{suffix}");
                    suffix += 1;
                }
                name
            });
        if selected.is_some() {
            match candidate.ai.providers.get(&provider_id) {
                Some(provider) if provider.is_ecs_identity() => {}
                Some(_) => return Err("provider_definition_changed".into()),
                None => return Err("provider_not_found".into()),
            }
        }
        if !candidate.ai.providers.contains_key(&provider_id) {
            let provider = ProviderConfig {
                provider_type: Some("aliyun".into()),
                auth_source: Some("ecs_identity".into()),
                model: Some(model.clone()),
                ..Default::default()
            };
            candidate
                .ai
                .providers
                .insert(provider_id.clone(), provider.clone());
            candidate
                .user_ai
                .providers
                .insert(provider_id.clone(), provider);
        }
        if let Some(provider) = candidate.user_ai.providers.get_mut(&provider_id) {
            provider.model = Some(model.clone());
        }
        if let Some(provider) = candidate.ai.providers.get_mut(&provider_id) {
            provider.model = Some(model.clone());
        }
        candidate.ai.active_provider = Some(provider_id.clone());
        candidate.user_ai.active_provider = Some(provider_id.clone());
        candidate.ai.active_model = Some(model.clone());
        candidate.user_ai.active_model = Some(model.clone());
        Ok(candidate
            .user_ai
            .providers
            .contains_key(&provider_id)
            .then_some(provider_id))
    });
    if let Err(failure) = persisted {
        if !previous_preference && save_preference(false).await.is_err() {
            return error(
                request_id,
                "trial_activation_save_failed_preference_restore_failed",
            );
        }
        let message = match failure.as_str() {
            "provider_definition_changed" => {
                "provider definition changed; reload authentication state and activate again"
            }
            "provider_not_found" => {
                "provider no longer exists; reload authentication state and activate again"
            }
            _ => return error(request_id, "trial_activation_save_failed"),
        };
        return OutputMessage::RegistryResponse {
            request_id: request_id.into(),
            success: false,
            data: Some(serde_json::json!({"error_code":failure})),
            error: Some(message.into()),
        };
    }
    OutputMessage::RegistryResponse {
        request_id: request_id.into(),
        success: true,
        data: Some(
            serde_json::json!({"active_provider":config.ai.active_provider,"model":model,"trial_preferences":{"auto_use":true}}),
        ),
        error: None,
    }
}
