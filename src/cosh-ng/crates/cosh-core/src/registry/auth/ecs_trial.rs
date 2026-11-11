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

pub(super) fn state(config: &CoreConfig, available: bool, auto_use: bool) -> Value {
    let active = available
        && trial_policy::decide(config, &ProviderIntent::from_env(), true, true, auto_use)
            == TrialDecision::EcsIdentity;
    let provider_id = saved_provider(config);
    serde_json::json!({"available":available,"active":active,"model":MODEL,"provider_id":provider_id})
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
    let mut candidate = config.clone();
    let model = MODEL.to_string();
    let provider_id = selected
        .or_else(|| saved_provider(config))
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
    if !previous_preference {
        if let Err(code) = save_preference(true).await {
            return error(request_id, &code);
        }
    }
    if crate::config::persist_config(&candidate).is_err() {
        if !previous_preference && save_preference(false).await.is_err() {
            return error(
                request_id,
                "trial_activation_save_failed_preference_restore_failed",
            );
        }
        return error(request_id, "trial_activation_save_failed");
    }
    *config = candidate;
    OutputMessage::RegistryResponse {
        request_id: request_id.into(),
        success: true,
        data: Some(
            serde_json::json!({"active_provider":provider_id,"model":model,"trial_preferences":{"auto_use":true}}),
        ),
        error: None,
    }
}
