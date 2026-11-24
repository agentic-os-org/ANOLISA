use super::{config_dir, guarded, replace_ai_section, AiConfig, CoreConfig};

pub(crate) fn persist_auth_provider(
    candidate: &mut CoreConfig,
    provider_id: &str,
    guard: bool,
) -> Result<(), String> {
    let provider = candidate
        .user_ai
        .providers
        .get(provider_id)
        .cloned()
        .ok_or("auth candidate is missing its provider")?;
    persist_auth_update(candidate, guard, |latest| {
        latest.session_providers.remove(provider_id);
        latest
            .ai
            .providers
            .insert(provider_id.to_string(), provider.clone());
        latest
            .user_ai
            .providers
            .insert(provider_id.to_string(), provider.clone());
        latest.ai.active_provider = Some(provider_id.to_string());
        latest.user_ai.active_provider = Some(provider_id.to_string());
        latest.ai.active_model.clone_from(&provider.model);
        latest.user_ai.active_model = provider.model;
        Ok(Some(provider_id.to_string()))
    })
}

pub(crate) fn persist_auth_update(
    config: &mut CoreConfig,
    guard: bool,
    update: impl FnOnce(&mut CoreConfig) -> Result<Option<String>, String>,
) -> Result<(), String> {
    let dir = config_dir();
    if guard {
        guarded::verify_dir(&dir)?;
    } else {
        std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    }
    let path = dir.join("config.toml");
    let _lock = guarded::lock(&dir, guard)?;
    let existing = if guard {
        guarded::verify_target(&path)?;
        guarded::read_existing(&path)?
    } else {
        match std::fs::read_to_string(&path) {
            Ok(content) => content,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.to_string()),
        }
    };
    let mut document: toml::Table =
        toml::from_str(&existing).map_err(|_| "configuration is not valid TOML".to_string())?;
    let ai = document
        .entry("ai")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or("ai must be a table")?;
    let user_ai: AiConfig = toml::Value::Table(ai.clone())
        .try_into()
        .map_err(|_| "AI configuration contains invalid values".to_string())?;
    let mut candidate = config.clone();
    reconcile_user_ai(&mut candidate, user_ai);
    if let Some(provider_id) = update(&mut candidate)? {
        let providers = ai
            .entry("providers")
            .or_insert_with(|| toml::Value::Table(toml::Table::new()))
            .as_table_mut()
            .ok_or("ai.providers must be a table")?;
        if let Some(provider) = candidate.user_ai.providers.get(&provider_id) {
            providers.insert(
                provider_id,
                toml::Value::try_from(provider)
                    .map_err(|_| "provider configuration could not be encoded".to_string())?,
            );
        } else {
            providers.remove(&provider_id);
        }
    }
    for (key, value) in [
        ("active_provider", &candidate.user_ai.active_provider),
        ("active_model", &candidate.user_ai.active_model),
    ] {
        if let Some(value) = value {
            ai.insert(key.to_string(), toml::Value::String(value.clone()));
        } else {
            ai.remove(key);
        }
    }
    let serialized = toml::to_string(&toml::Table::from_iter([(
        "ai".to_string(),
        toml::Value::Table(ai.clone()),
    )]))
    .map_err(|_| "AI configuration could not be encoded".to_string())?;
    replace_ai_section(&existing, &serialized, &dir, guard)?;
    *config = candidate;
    Ok(())
}

fn reconcile_user_ai(config: &mut CoreConfig, user_ai: AiConfig) {
    for provider_id in config.user_ai.providers.keys() {
        if let Some(provider) = config.system_ai.providers.get(provider_id) {
            config
                .ai
                .providers
                .insert(provider_id.clone(), provider.clone());
        } else {
            config.ai.providers.remove(provider_id);
        }
    }
    config.ai.providers.extend(user_ai.providers.clone());
    config.session_providers.retain(|provider_id, _| {
        !config.user_ai.providers.contains_key(provider_id)
            || user_ai.providers.contains_key(provider_id)
    });
    config.ai.providers.extend(config.session_providers.clone());
    config.user_ai = user_ai;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ProviderConfig;

    #[test]
    fn reconciliation_removes_user_credentials_and_restores_system_provider() {
        let mut config = CoreConfig::default();
        config.user_ai.providers.insert(
            "shared".into(),
            ProviderConfig {
                api_key: Some("old-user-secret".into()),
                ..Default::default()
            },
        );
        config
            .user_ai
            .providers
            .insert("deleted".into(), ProviderConfig::default());
        config.ai.providers = config.user_ai.providers.clone();
        config.system_ai.providers.insert(
            "shared".into(),
            ProviderConfig {
                api_key: Some("system-secret".into()),
                ..Default::default()
            },
        );
        config
            .ai
            .providers
            .insert("runtime".into(), ProviderConfig::default());
        config.ai.output_language = Some("runtime-language".into());
        config.ai.thinking = Some("runtime-thinking".into());
        config.ai.active_provider = Some("shared".into());
        config.user_ai.active_provider = Some("shared".into());
        config.system_ai.active_provider = Some("system-default".into());
        reconcile_user_ai(&mut config, AiConfig::default());
        assert!(!config.ai.providers.contains_key("deleted"));
        assert_eq!(
            config.ai.providers["shared"].api_key.as_deref(),
            Some("system-secret")
        );
        assert!(config.ai.providers.contains_key("runtime"));
        assert_eq!(
            config.ai.output_language.as_deref(),
            Some("runtime-language")
        );
        assert_eq!(config.ai.thinking.as_deref(), Some("runtime-thinking"));
        assert_eq!(config.ai.active_provider.as_deref(), Some("shared"));
        assert!(config.user_ai.providers.is_empty());
    }

    #[test]
    fn reconciliation_keeps_explicit_runtime_selection() {
        let mut config = CoreConfig::default();
        config.ai.active_provider = Some("old-user".into());
        config.ai.active_model = Some("old-model".into());
        config.user_ai.active_provider = Some("old-user".into());
        config.user_ai.active_model = Some("old-model".into());
        let user_ai = AiConfig {
            active_provider: Some("new-user".into()),
            active_model: Some("new-model".into()),
            ..Default::default()
        };
        reconcile_user_ai(&mut config, user_ai);
        assert_eq!(config.ai.active_provider.as_deref(), Some("old-user"));
        assert_eq!(config.ai.active_model.as_deref(), Some("old-model"));
    }

    #[test]
    fn reconciliation_keeps_session_override_until_its_user_provider_is_deleted() {
        let mut config = CoreConfig::default();
        let disk_provider = crate::config::ProviderConfig {
            api_key: Some("old-disk-key".into()),
            ..Default::default()
        };
        let session_provider = crate::config::ProviderConfig {
            api_key: Some("session-key".into()),
            ..Default::default()
        };
        config
            .user_ai
            .providers
            .insert("provider".into(), disk_provider.clone());
        config
            .ai
            .providers
            .insert("provider".into(), session_provider.clone());
        config
            .session_providers
            .insert("provider".into(), session_provider);
        let latest = AiConfig {
            providers: std::collections::HashMap::from([("provider".into(), disk_provider)]),
            ..Default::default()
        };
        reconcile_user_ai(&mut config, latest);
        assert_eq!(
            config.ai.providers["provider"].api_key.as_deref(),
            Some("session-key")
        );
        assert_eq!(
            config.user_ai.providers["provider"].api_key.as_deref(),
            Some("old-disk-key")
        );
        reconcile_user_ai(&mut config, AiConfig::default());
        assert!(!config.ai.providers.contains_key("provider"));
        assert!(!config.session_providers.contains_key("provider"));
    }
}
