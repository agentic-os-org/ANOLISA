//! Candidate-generation validation, settings transactions, and runtime publication.

use super::*;

pub(super) fn apply_live_runtime_projection(
    value: &mut Value,
    live_runtime: Option<&LiveExtensionRuntime>,
    extension: Option<&str>,
) {
    let Some(live_runtime) = live_runtime else {
        return;
    };
    let runtime = live_runtime.projection(extension);
    let Value::Object(fields) = value else {
        return;
    };
    if let Some(effective_state) = runtime.get("effective_state") {
        fields.insert("effective_state".to_string(), effective_state.clone());
    }
    if let Some(is_active) = runtime.get("is_active") {
        fields.insert("is_active".to_string(), is_active.clone());
    }
    if let Some(health) = runtime.get("health").filter(|health| !health.is_null()) {
        fields.insert("health".to_string(), health.clone());
    }
    fields.insert(
        "activation".to_string(),
        Value::String("current".to_string()),
    );
    if let Some(generation) = runtime.get("generation") {
        fields.insert("generation".to_string(), generation.clone());
    }
    fields.insert("runtime".to_string(), runtime);
}

pub(super) async fn commit_with_candidate_health(
    installer: &ExtensionInstaller,
    operation_id: &str,
    fingerprint: &str,
    config: &CoreConfig,
    ext_manager: &mut ExtensionManager,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> Result<
    (
        crate::extension::installer::ExtensionMutationResult,
        Option<RuntimePublication>,
    ),
    String,
> {
    let pending = installer
        .commit_pending(operation_id, fingerprint)
        .map_err(|error| format!("{}: {error}", error.code()))?;
    if !pending.result().changed {
        return installer
            .finalize_pending(pending)
            .map(|result| (result, None))
            .map_err(|error| format!("{}: {error}", error.code()));
    }

    ext_manager.refresh();
    let next_generation = candidate_generation(live_runtime);
    let workspace = ext_manager.workspace_dir().to_path_buf();
    let builder = RuntimeSnapshotBuilder::new(ext_manager, config, workspace, next_generation);
    let builder = match live_runtime {
        Some(runtime) => runtime.configure_builder(builder),
        None => builder,
    };
    let snapshot = builder.build().await;
    if !snapshot.generation.healthy {
        let diagnostic_codes = snapshot
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>()
            .join(",");
        snapshot.mcp.shutdown().await;
        let rollback = installer.rollback_pending(pending);
        ext_manager.refresh();
        return match rollback {
            Ok(()) => Err(format!(
                "extension_candidate_validation_failed: candidate runtime is unhealthy ({diagnostic_codes})"
            )),
            Err(error) => Err(format!(
                "extension_candidate_rollback_failed: candidate unhealthy ({diagnostic_codes}); {}: {error}",
                error.code()
            )),
        };
    }

    let result = match installer
        .finalize_pending(pending)
        .map_err(|error| format!("{}: {error}", error.code()))
    {
        Ok(result) => result,
        Err(error) => {
            snapshot.mcp.shutdown().await;
            return Err(error);
        }
    };
    let publication = publish_or_shutdown(snapshot, live_runtime).await?;
    Ok((result, publication))
}

pub(super) async fn uninstall_with_candidate_health(
    installer: &ExtensionInstaller,
    name: &str,
    config: &CoreConfig,
    ext_manager: &mut ExtensionManager,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> Result<
    (
        crate::extension::installer::ExtensionMutationResult,
        Option<RuntimePublication>,
    ),
    String,
> {
    let pending = installer
        .uninstall_pending(name)
        .map_err(|error| format!("{}: {error}", error.code()))?;
    ext_manager.refresh();
    let next_generation = candidate_generation(live_runtime);
    let workspace = ext_manager.workspace_dir().to_path_buf();
    let builder = RuntimeSnapshotBuilder::new(ext_manager, config, workspace, next_generation);
    let builder = match live_runtime {
        Some(runtime) => runtime.configure_builder(builder),
        None => builder,
    };
    let snapshot = builder.build().await;
    if !snapshot.generation.healthy {
        let diagnostic_codes = snapshot
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>()
            .join(",");
        snapshot.mcp.shutdown().await;
        let rollback = installer.rollback_uninstall(pending);
        ext_manager.refresh();
        return match rollback {
            Ok(()) => Err(format!(
                "extension_candidate_validation_failed: uninstall candidate is unhealthy ({diagnostic_codes})"
            )),
            Err(error) => Err(format!(
                "extension_candidate_rollback_failed: uninstall candidate unhealthy ({diagnostic_codes}); {}: {error}",
                error.code()
            )),
        };
    }

    let result = match installer
        .finalize_uninstall(pending)
        .map_err(|error| format!("{}: {error}", error.code()))
    {
        Ok(result) => result,
        Err(error) => {
            snapshot.mcp.shutdown().await;
            return Err(error);
        }
    };
    let publication = publish_or_shutdown(snapshot, live_runtime).await?;
    Ok((result, publication))
}

pub(super) enum CatalogStateMutation {
    SetEnabled {
        name: String,
        enabled: bool,
    },
    SelectSource {
        name: String,
        selection: crate::extension::state::SourceSelection,
    },
}

impl CatalogStateMutation {
    fn name(&self) -> &str {
        match self {
            Self::SetEnabled { name, .. } | Self::SelectSource { name, .. } => name,
        }
    }
}

pub(super) async fn catalog_state_with_candidate_health(
    installer: &ExtensionInstaller,
    mutation: CatalogStateMutation,
    config: &CoreConfig,
    ext_manager: &mut ExtensionManager,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> Result<(crate::extension::Extension, Option<RuntimePublication>), String> {
    let target = mutation.name().to_string();
    let pending = installer
        .begin_state_mutation()
        .map_err(|error| format!("{}: {error}", error.code()))?;
    let applied = match mutation {
        CatalogStateMutation::SetEnabled { name, enabled } => {
            ext_manager.set_enabled(&name, enabled)
        }
        CatalogStateMutation::SelectSource { name, selection } => {
            ext_manager.select_source(&name, selection)
        }
    };
    if let Err(error) = applied {
        let rollback = installer.rollback_state_mutation(pending);
        ext_manager.refresh();
        return match rollback {
            Ok(()) => Err(error),
            Err(rollback) => Err(format!("{error}; {}: {rollback}", rollback.code())),
        };
    }

    let next_generation = candidate_generation(live_runtime);
    let workspace = ext_manager.workspace_dir().to_path_buf();
    let builder = RuntimeSnapshotBuilder::new(ext_manager, config, workspace, next_generation);
    let builder = match live_runtime {
        Some(runtime) => runtime.configure_builder(builder),
        None => builder,
    };
    let snapshot = builder.build().await;
    if !snapshot.generation.healthy {
        let diagnostic_codes = snapshot
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>()
            .join(",");
        snapshot.mcp.shutdown().await;
        let rollback = installer.rollback_state_mutation(pending);
        ext_manager.refresh();
        return match rollback {
            Ok(()) => Err(format!(
                "extension_candidate_validation_failed: state candidate is unhealthy ({diagnostic_codes})"
            )),
            Err(error) => Err(format!(
                "extension_candidate_rollback_failed: state candidate unhealthy ({diagnostic_codes}); {}: {error}",
                error.code()
            )),
        };
    }

    if let Err(error) = installer
        .finalize_state_mutation(pending)
        .map_err(|error| format!("{}: {error}", error.code()))
    {
        snapshot.mcp.shutdown().await;
        return Err(error);
    }
    let publication = publish_or_shutdown(snapshot, live_runtime).await?;
    let extension = ext_manager
        .list()
        .iter()
        .find(|extension| extension.name == target)
        .cloned()
        .ok_or_else(|| format!("extension not found after state mutation: {target}"))?;
    Ok((extension, publication))
}

pub(super) fn extension_agent_registry(
    ext_manager: &ExtensionManager,
    config: &CoreConfig,
) -> crate::extension::AgentRegistry {
    let allowed_tools = [
        "shell",
        "read_file",
        "write_file",
        "edit",
        "grep",
        "todo",
        "skill",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let workspace_trusted = ExtensionSettings::new(ext_manager.workspace_dir().to_path_buf())
        .map(|settings| settings.workspace_trusted())
        .unwrap_or(false);
    crate::extension::AgentRegistry::build(
        ext_manager,
        &allowed_tools,
        workspace_trusted,
        config.agent.approval_mode,
    )
}

pub(super) fn registry_success(request_id: &str, data: Value) -> OutputMessage {
    OutputMessage::RegistryResponse {
        request_id: request_id.to_string(),
        success: true,
        data: Some(data),
        error: None,
    }
}

pub(super) fn extension_installer_error(
    request_id: &str,
    error: &crate::extension::installer::InstallerError,
) -> OutputMessage {
    registry_error(request_id, &format!("{}: {error}", error.code()))
}

pub(super) fn extension_scaffold_error(
    request_id: &str,
    error: &crate::extension::scaffold::ScaffoldError,
) -> OutputMessage {
    registry_error(request_id, &format!("{}: {error}", error.code()))
}

pub(super) fn extension_settings_error(request_id: &str, error: &SettingsError) -> OutputMessage {
    registry_error(request_id, &format!("{}: {error}", error.code()))
}

pub(super) async fn handle_extension_settings(
    request_id: &str,
    action: &str,
    params: &Value,
    config: &CoreConfig,
    ext_manager: &mut ExtensionManager,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> OutputMessage {
    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
    if name.is_empty() {
        return registry_error(request_id, "missing 'name' parameter");
    }
    let Some(extension) = ext_manager
        .list()
        .iter()
        .find(|extension| extension.name == name)
        .cloned()
    else {
        return registry_error(request_id, &format!("extension not found: {name}"));
    };
    let settings = match ExtensionSettings::new(ext_manager.workspace_dir().to_path_buf()) {
        Ok(settings) => settings,
        Err(error) => return extension_settings_error(request_id, &error),
    };
    let scope = match params.get("scope").and_then(Value::as_str) {
        Some(scope) => match SettingScope::parse(scope) {
            Ok(scope) => Some(scope),
            Err(error) => return extension_settings_error(request_id, &error),
        },
        None => None,
    };
    let result = match action {
        "settings-list" => match scope {
            Some(scope) => settings
                .list_scoped(&extension, scope)
                .and_then(|views| serde_json::to_value(views).map_err(settings_serialize_error)),
            None => settings
                .list(&extension)
                .and_then(|views| serde_json::to_value(views).map_err(settings_serialize_error)),
        },
        "settings-get" => {
            let key = params.get("key").and_then(Value::as_str).unwrap_or("");
            if key.is_empty() {
                return registry_error(request_id, "missing 'key' parameter");
            }
            match scope {
                Some(scope) => settings.get_scoped(&extension, key, scope),
                None => settings.get(&extension, key),
            }
            .and_then(|view| serde_json::to_value(view).map_err(settings_serialize_error))
        }
        "settings-set" => {
            let key = params.get("key").and_then(Value::as_str).unwrap_or("");
            let value = params.get("value").and_then(Value::as_str);
            if key.is_empty() || value.is_none() || scope.is_none() {
                return registry_error(
                    request_id,
                    "settings-set requires 'key', 'value', and 'scope' parameters",
                );
            }
            return settings_mutation_with_candidate_health(
                request_id,
                &settings,
                &extension,
                (key, scope.unwrap(), Some(value.unwrap_or_default())),
                config,
                ext_manager,
                live_runtime,
            )
            .await;
        }
        "settings-unset" => {
            let key = params.get("key").and_then(Value::as_str).unwrap_or("");
            if key.is_empty() || scope.is_none() {
                return registry_error(
                    request_id,
                    "settings-unset requires 'key' and 'scope' parameters",
                );
            }
            return settings_mutation_with_candidate_health(
                request_id,
                &settings,
                &extension,
                (key, scope.unwrap(), None),
                config,
                ext_manager,
                live_runtime,
            )
            .await;
        }
        _ => unreachable!("settings action dispatched by the caller"),
    };
    match result {
        Ok(data) => registry_success(request_id, data),
        Err(error) => extension_settings_error(request_id, &error),
    }
}

pub(super) async fn settings_mutation_with_candidate_health(
    request_id: &str,
    settings: &ExtensionSettings,
    extension: &crate::extension::Extension,
    mutation: (&str, SettingScope, Option<&str>),
    config: &CoreConfig,
    ext_manager: &mut ExtensionManager,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> OutputMessage {
    let (key, scope, value) = mutation;
    let installer = match ExtensionInstaller::for_current_user() {
        Ok(installer) => installer,
        Err(error) => return extension_installer_error(request_id, &error),
    };
    let store_lock = match installer.begin_settings_mutation() {
        Ok(lock) => lock,
        Err(error) => return extension_installer_error(request_id, &error),
    };
    let operation_id = store_lock.operation_id().to_string();
    let pending = match value {
        Some(value) => settings.begin_set(&operation_id, extension, key, value, scope),
        None => settings.begin_unset(&operation_id, extension, key, scope),
    };
    let pending = match pending {
        Ok(pending) => pending,
        Err(error) => {
            installer.finish_settings_mutation(store_lock);
            return extension_settings_error(request_id, &error);
        }
    };
    let candidate_settings = settings.with_candidate(&pending);
    let next_generation = candidate_generation(live_runtime);
    let workspace = ext_manager.workspace_dir().to_path_buf();
    let builder = RuntimeSnapshotBuilder::new(ext_manager, config, workspace, next_generation)
        .with_settings(&candidate_settings);
    let builder = match live_runtime {
        Some(runtime) => runtime.configure_builder(builder),
        None => builder,
    };
    let snapshot = builder.build().await;
    if !snapshot.generation.healthy {
        let diagnostic_codes = snapshot
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code.as_str())
            .collect::<Vec<_>>()
            .join(",");
        snapshot.mcp.shutdown().await;
        let rollback = settings.rollback(pending);
        installer.finish_settings_mutation(store_lock);
        ext_manager.refresh();
        return match rollback {
            Ok(()) => registry_error(
                request_id,
                &format!(
                    "extension_candidate_validation_failed: settings candidate is unhealthy ({diagnostic_codes})"
                ),
            ),
            Err(error) => registry_error(
                request_id,
                &format!(
                    "extension_candidate_rollback_failed: settings candidate unhealthy ({diagnostic_codes}); {}: {error}",
                    error.code()
                ),
            ),
        };
    }

    let diagnostics = snapshot.diagnostics.clone();
    let setting = match settings.commit(pending, extension) {
        Ok(setting) => setting,
        Err(error) => {
            snapshot.mcp.shutdown().await;
            installer.finish_settings_mutation(store_lock);
            ext_manager.refresh();
            return extension_settings_error(request_id, &error);
        }
    };
    installer.finish_settings_mutation(store_lock);
    let publication = match publish_or_shutdown(snapshot, live_runtime).await {
        Ok(publication) => publication,
        Err(error) => {
            ext_manager.refresh();
            return registry_error(request_id, &error);
        }
    };
    ext_manager.refresh();
    with_runtime_publication(
        registry_success(
            request_id,
            serde_json::json!({
                "activation": "pending_safe_reload",
                "candidate_generation": next_generation,
                "diagnostics": diagnostics,
                "health": "healthy",
                "operation_id": operation_id,
                "setting": setting,
                "warnings": [],
            }),
        ),
        publication.as_ref(),
    )
}

pub(super) fn candidate_generation(live_runtime: Option<&LiveExtensionRuntime>) -> u64 {
    live_runtime.map_or_else(
        || {
            crate::extension::state::load(None)
                .map(|loaded| loaded.state.active_generation.saturating_add(1))
                .unwrap_or(1)
        },
        LiveExtensionRuntime::next_generation,
    )
}

pub(super) async fn publish_or_shutdown(
    snapshot: RuntimeSnapshot,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> Result<Option<RuntimePublication>, String> {
    match live_runtime {
        Some(live_runtime) => live_runtime.publish(snapshot).await.map(Some),
        None => {
            snapshot.mcp.shutdown().await;
            Ok(None)
        }
    }
}

pub(super) fn runtime_publication_value(publication: RuntimePublication) -> Value {
    serde_json::json!({
        "activation": publication.activation,
        "candidate_generation": publication.candidate_generation,
        "current_generation": publication.current_generation,
        "active_runs": publication.active_runs,
        "pending": publication.pending,
    })
}

pub(super) fn with_runtime_publication(
    mut response: OutputMessage,
    publication: Option<&RuntimePublication>,
) -> OutputMessage {
    let Some(publication) = publication else {
        return response;
    };
    if let OutputMessage::RegistryResponse {
        success: true,
        data: Some(Value::Object(data)),
        ..
    } = &mut response
    {
        data.insert(
            "activation".to_string(),
            Value::String(publication.activation.to_string()),
        );
        data.insert(
            "candidate_generation".to_string(),
            Value::from(publication.candidate_generation),
        );
        data.insert(
            "current_generation".to_string(),
            Value::from(publication.current_generation),
        );
        data.insert(
            "active_runs".to_string(),
            Value::from(publication.active_runs),
        );
        data.insert("pending".to_string(), Value::Bool(publication.pending));
    }
    response
}
