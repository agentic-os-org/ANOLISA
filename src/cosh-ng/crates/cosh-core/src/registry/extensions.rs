//! Extension registry commands and candidate-generation transactions.

use super::*;

mod runtime;

use runtime::*;

pub(super) async fn handle_extensions(
    request_id: &str,
    action: &str,
    params: &Value,
    config: &CoreConfig,
    ext_manager: &mut ExtensionManager,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> OutputMessage {
    match action {
        "list" => {
            let extensions: Vec<Value> = ext_manager
                .list()
                .iter()
                .map(|extension| {
                    let mut value = extension_registry_value(extension);
                    apply_live_runtime_projection(&mut value, live_runtime, Some(&extension.name));
                    value
                })
                .collect();
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(Value::Array(extensions)),
                error: None,
            }
        }
        "detail" | "info" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            match ext_manager.list().iter().find(|e| e.name == name) {
                Some(ext) => {
                    let mut detail = extension_registry_value(ext);
                    if let Value::Object(fields) = &mut detail {
                        fields.insert(
                            "has_hooks".to_string(),
                            Value::Bool(!ext.config.hooks.is_empty()),
                        );
                        fields.insert(
                            "skill_dirs".to_string(),
                            serde_json::json!(ext.config.skills.0),
                        );
                        let agents = extension_agent_registry(ext_manager, config);
                        fields.insert(
                            "agents".to_string(),
                            serde_json::json!(agents
                                .list()
                                .iter()
                                .filter(|agent| agent.id.starts_with(&format!("{name}/agent/")))
                                .collect::<Vec<_>>()),
                        );
                    }
                    apply_live_runtime_projection(&mut detail, live_runtime, Some(name));
                    OutputMessage::RegistryResponse {
                        request_id: request_id.to_string(),
                        success: true,
                        data: Some(detail),
                        error: None,
                    }
                }
                None => OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some(format!("extension not found: {name}")),
                },
            }
        }
        "enable" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some("missing 'name' parameter".to_string()),
                };
            }
            // Cleanup: remove extension's hooks from hooks.json disabled list
            let hook_names = ext_manager.extension_hook_names(name);
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match catalog_state_with_candidate_health(
                &installer,
                CatalogStateMutation::SetEnabled {
                    name: name.to_string(),
                    enabled: true,
                },
                config,
                ext_manager,
                live_runtime,
            )
            .await
            {
                Ok((extension, publication)) => {
                    if !hook_names.is_empty() {
                        let _ = crate::state::remove_disabled_set(
                            crate::state::HOOKS_STATE,
                            &hook_names,
                        );
                    }
                    with_runtime_publication(
                        extension_mutation_response(request_id, "enable", &extension),
                        publication.as_ref(),
                    )
                }
                Err(error) => {
                    registry_error(request_id, &format!("failed to enable extension: {error}"))
                }
            }
        }
        "disable" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            if name.is_empty() {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some("missing 'name' parameter".to_string()),
                };
            }
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match catalog_state_with_candidate_health(
                &installer,
                CatalogStateMutation::SetEnabled {
                    name: name.to_string(),
                    enabled: false,
                },
                config,
                ext_manager,
                live_runtime,
            )
            .await
            {
                Ok((extension, publication)) => with_runtime_publication(
                    extension_mutation_response(request_id, "disable", &extension),
                    publication.as_ref(),
                ),
                Err(error) => {
                    registry_error(request_id, &format!("failed to disable extension: {error}"))
                }
            }
        }
        "select-source" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let source = params.get("source").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() || source.is_empty() {
                return registry_error(request_id, "missing 'name' or 'source' parameter");
            }
            let selection = match source {
                "user" => crate::extension::state::SourceSelection::User,
                "system" => crate::extension::state::SourceSelection::System,
                _ => return registry_error(request_id, "source must be 'user' or 'system'"),
            };
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match catalog_state_with_candidate_health(
                &installer,
                CatalogStateMutation::SelectSource {
                    name: name.to_string(),
                    selection,
                },
                config,
                ext_manager,
                live_runtime,
            )
            .await
            {
                Ok((extension, publication)) => with_runtime_publication(
                    extension_mutation_response(request_id, "select-source", &extension),
                    publication.as_ref(),
                ),
                Err(error) => registry_error(
                    request_id,
                    &format!("failed to select extension source: {error}"),
                ),
            }
        }
        "settings-list" | "settings-get" | "settings-set" | "settings-unset" => {
            handle_extension_settings(
                request_id,
                action,
                params,
                config,
                ext_manager,
                live_runtime,
            )
            .await
        }
        "install-preflight" | "link-preflight" => {
            let source = params.get("source").and_then(Value::as_str).unwrap_or("");
            if source.is_empty() {
                return registry_error(request_id, "missing 'source' parameter");
            }
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            let source_kind = params
                .get("source_kind")
                .and_then(Value::as_str)
                .unwrap_or("path-copy");
            let result = if action == "link-preflight" {
                installer.preflight_link(std::path::Path::new(source))
            } else if source_kind == "git-https" {
                installer.preflight_git_install(source, params.get("ref").and_then(Value::as_str))
            } else if source_kind == "path-copy" {
                installer.preflight_path_copy(std::path::Path::new(source))
            } else {
                return registry_error(
                    request_id,
                    "source_kind must be 'path-copy' or 'git-https'",
                );
            };
            match result {
                Ok(preflight) => registry_success(request_id, serde_json::json!(preflight)),
                Err(error) => extension_installer_error(request_id, &error),
            }
        }
        "update-preflight" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                return registry_error(request_id, "missing 'name' parameter");
            }
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match installer.preflight_update(name) {
                Ok(preflight) => registry_success(request_id, serde_json::json!(preflight)),
                Err(error) => extension_installer_error(request_id, &error),
            }
        }
        "update-all-preflight" => {
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            let items = ext_manager
                .list()
                .iter()
                .map(|extension| extension.name.clone())
                .collect::<Vec<_>>();
            match installer.begin_batch("update-all", &items) {
                Ok(batch) => registry_success(request_id, batch),
                Err(error) => extension_installer_error(request_id, &error),
            }
        }
        "update-all-commit" => {
            let operation_id = params
                .get("operation_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if operation_id.is_empty() {
                return registry_error(request_id, "missing 'operation_id' parameter");
            }
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match execute_update_all(&installer, operation_id, config, ext_manager, live_runtime)
                .await
            {
                Ok(result) => registry_success(request_id, result),
                Err(error) => registry_error(request_id, &error),
            }
        }
        "new" => {
            let path = params.get("path").and_then(Value::as_str).unwrap_or("");
            let template = params
                .get("template")
                .and_then(Value::as_str)
                .unwrap_or("minimal");
            if path.is_empty() {
                return registry_error(request_id, "missing 'path' parameter");
            }
            let template = match ExtensionTemplate::parse(template) {
                Ok(template) => template,
                Err(error) => return extension_scaffold_error(request_id, &error),
            };
            match scaffold_extension(std::path::Path::new(path), template) {
                Ok(result) => registry_success(request_id, serde_json::json!(result)),
                Err(error) => extension_scaffold_error(request_id, &error),
            }
        }
        "operation" => {
            let operation_id = params
                .get("operation_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if operation_id.is_empty() {
                return registry_error(request_id, "missing 'operation_id' parameter");
            }
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match installer.operation(operation_id) {
                Ok(preflight) => registry_success(request_id, serde_json::json!(preflight)),
                Err(error) => extension_installer_error(request_id, &error),
            }
        }
        "result" => {
            let operation_id = params
                .get("operation_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if operation_id.is_empty() {
                return registry_error(request_id, "missing 'operation_id' parameter");
            }
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match installer.result_value(operation_id) {
                Ok(result) => registry_success(request_id, result),
                Err(error) => extension_installer_error(request_id, &error),
            }
        }
        "commit" => {
            let operation_id = params
                .get("operation_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            let fingerprint = params
                .get("fingerprint")
                .and_then(Value::as_str)
                .unwrap_or("");
            if operation_id.is_empty() || fingerprint.is_empty() {
                return registry_error(
                    request_id,
                    "missing 'operation_id' or 'fingerprint' parameter",
                );
            }
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match commit_with_candidate_health(
                &installer,
                operation_id,
                fingerprint,
                config,
                ext_manager,
                live_runtime,
            )
            .await
            {
                Ok((result, publication)) => {
                    ext_manager.refresh();
                    with_runtime_publication(
                        extension_lifecycle_mutation_response(request_id, &result, ext_manager),
                        publication.as_ref(),
                    )
                }
                Err(error) => registry_error(request_id, &error),
            }
        }
        "cancel" => {
            let operation_id = params
                .get("operation_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if operation_id.is_empty() {
                return registry_error(request_id, "missing 'operation_id' parameter");
            }
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match installer.cancel(operation_id) {
                Ok(()) => registry_success(
                    request_id,
                    serde_json::json!({"operation_id": operation_id, "cancelled": true}),
                ),
                Err(error) => extension_installer_error(request_id, &error),
            }
        }
        "uninstall" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if name.is_empty() {
                return registry_error(request_id, "missing 'name' parameter");
            }
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match uninstall_with_candidate_health(
                &installer,
                name,
                config,
                ext_manager,
                live_runtime,
            )
            .await
            {
                Ok((result, publication)) => {
                    ext_manager.refresh();
                    with_runtime_publication(
                        extension_lifecycle_mutation_response(request_id, &result, ext_manager),
                        publication.as_ref(),
                    )
                }
                Err(error) => registry_error(request_id, &error),
            }
        }
        "recover" => {
            let installer = match ExtensionInstaller::for_current_user() {
                Ok(installer) => installer,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            match installer.recover() {
                Ok(result) => {
                    ext_manager.refresh();
                    registry_success(request_id, serde_json::json!(result))
                }
                Err(error) => extension_installer_error(request_id, &error),
            }
        }
        "reload" => {
            ext_manager.refresh();
            if let Some(live_runtime) = live_runtime {
                let next_generation = live_runtime.next_generation();
                let workspace = ext_manager.workspace_dir().to_path_buf();
                let snapshot = live_runtime
                    .configure_builder(RuntimeSnapshotBuilder::new(
                        ext_manager,
                        config,
                        workspace,
                        next_generation,
                    ))
                    .build()
                    .await;
                if !snapshot.generation.healthy {
                    let diagnostics = snapshot.diagnostics.clone();
                    snapshot.mcp.shutdown().await;
                    return registry_error(
                        request_id,
                        &format!(
                            "extension_runtime_unhealthy: candidate generation {next_generation} failed: {}",
                            diagnostics
                                .iter()
                                .map(|diagnostic| diagnostic.code.as_str())
                                .collect::<Vec<_>>()
                                .join(",")
                        ),
                    );
                }
                let diagnostics = snapshot.diagnostics.clone();
                let mcp_servers = snapshot.mcp.statuses().to_vec();
                return match live_runtime.publish(snapshot).await {
                    Ok(publication) => registry_success(
                        request_id,
                        serde_json::json!({
                            "action": "reload",
                            "activation": publication.activation,
                            "generation": publication.candidate_generation,
                            "current_generation": publication.current_generation,
                            "active_runs": publication.active_runs,
                            "pending": publication.pending,
                            "health": "healthy",
                            "runtime_diagnostics": diagnostics,
                            "mcp_servers": mcp_servers,
                            "warnings": [],
                        }),
                    ),
                    Err(error) => registry_error(request_id, &error),
                };
            }
            let runtime_context = crate::extension::ExtensionContextSnapshot::build(ext_manager);
            let mcp_runtime = McpRuntime::start(ext_manager).await;
            let mcp_statuses = mcp_runtime.statuses().to_vec();
            let mcp_diagnostics = mcp_runtime.diagnostics().to_vec();
            let candidate_healthy = ext_manager.list().iter().all(|extension| {
                extension.desired_state == crate::extension::DesiredState::Disabled
                    || matches!(
                        extension.health,
                        crate::extension::ExtensionHealth::Healthy
                            | crate::extension::ExtensionHealth::Degraded
                    )
            });
            mcp_runtime.shutdown().await;
            let generation = crate::extension::state::load(None)
                .map(|loaded| loaded.state.active_generation)
                .unwrap_or_default();
            registry_success(
                request_id,
                serde_json::json!({
                    "action": "reload",
                    "activation": "next_session",
                    "generation": generation,
                    "health": if candidate_healthy { "healthy" } else { "broken" },
                    "runtime_diagnostics": runtime_context.diagnostics(),
                    "mcp_diagnostics": mcp_diagnostics,
                    "mcp_servers": mcp_statuses,
                    "warnings": ["registry process cannot prove a safe live-session reload"],
                }),
            )
        }
        "doctor" => {
            let recovery = match ExtensionInstaller::for_current_user()
                .and_then(|installer| installer.recover())
            {
                Ok(recovery) => recovery,
                Err(error) => return extension_installer_error(request_id, &error),
            };
            ext_manager.refresh();
            let runtime_context = crate::extension::ExtensionContextSnapshot::build(ext_manager);
            let mcp_runtime = McpRuntime::start(ext_manager).await;
            let mcp_statuses = mcp_runtime.statuses().to_vec();
            let mcp_diagnostics = mcp_runtime.diagnostics().to_vec();
            let agents = extension_agent_registry(ext_manager, config);
            mcp_runtime.shutdown().await;
            let live = live_runtime.map(|runtime| runtime.projection(None));
            let diagnostics = ext_manager
                .catalog_diagnostics()
                .iter()
                .chain(
                    ext_manager
                        .list()
                        .iter()
                        .flat_map(|extension| extension.diagnostics.iter()),
                )
                .cloned()
                .collect::<Vec<_>>();
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(serde_json::json!({
                "extensions": ext_manager
                    .list()
                    .iter()
                    .map(extension_registry_value)
                    .collect::<Vec<_>>(),
                "diagnostics": diagnostics,
                "runtime_diagnostics": runtime_context.diagnostics(),
                "mcp_diagnostics": mcp_diagnostics,
                "mcp_servers": mcp_statuses,
                "agent_diagnostics": agents.diagnostics(),
                "agents": agents.list(),
                    "runtime": live,
                    "recovery": recovery,
                })),
                error: None,
            }
        }
        _ => OutputMessage::RegistryResponse {
            request_id: request_id.to_string(),
            success: false,
            data: None,
            error: Some(format!("unsupported action for extensions: {action}")),
        },
    }
}

async fn execute_update_all(
    installer: &ExtensionInstaller,
    operation_id: &str,
    config: &CoreConfig,
    ext_manager: &mut ExtensionManager,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> Result<Value, String> {
    let batch = installer
        .result_value(operation_id)
        .map_err(|error| format!("{}: {error}", error.code()))?;
    if batch.get("action").and_then(Value::as_str) != Some("update-all") {
        return Err("extension_batch_action_mismatch: operation is not update-all".to_string());
    }
    let status = batch.get("status").and_then(Value::as_str).unwrap_or("");
    if status != "prepared" {
        return Ok(batch);
    }
    let planned_items = batch
        .get("planned_items")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut outcomes = Vec::new();
    let mut checkpoint =
        update_all_checkpoint(operation_id, "in_progress", &planned_items, &outcomes);
    installer
        .write_batch_result(operation_id, &checkpoint)
        .map_err(|error| format!("{}: {error}", error.code()))?;

    for planned in &planned_items {
        let name = planned.as_str().unwrap_or_default();
        ext_manager.refresh();
        let extension = ext_manager
            .list()
            .iter()
            .find(|extension| extension.name == name)
            .cloned();
        let outcome = match extension {
            None => serde_json::json!({
                "name": name,
                "outcome": "failed",
                "code": "extension_not_found",
                "message": "extension disappeared after batch preflight",
            }),
            Some(extension)
                if extension.source != crate::extension::ExtensionSourceKind::GitHttps =>
            {
                serde_json::json!({
                    "name": name,
                    "outcome": "skipped",
                    "reason": "extension_source_not_updatable",
                })
            }
            Some(extension) => match installer.preflight_update(&extension.name) {
                Ok(preflight) if !preflight.consent_required => match commit_with_candidate_health(
                    installer,
                    &preflight.operation_id,
                    &preflight.capability_fingerprint,
                    config,
                    ext_manager,
                    live_runtime,
                )
                .await
                {
                    Ok((result, publication)) => serde_json::json!({
                        "name": extension.name,
                        "outcome": if result.changed { "updated" } else { "unchanged" },
                        "result": result,
                        "runtime": publication.map(runtime_publication_value),
                    }),
                    Err(error) => serde_json::json!({
                        "name": extension.name,
                        "outcome": "failed",
                        "code": "extension_candidate_validation_failed",
                        "message": error,
                    }),
                },
                Ok(preflight) => serde_json::json!({
                    "name": extension.name,
                    "outcome": "pending_consent",
                    "preflight": preflight,
                }),
                Err(error) => serde_json::json!({
                    "name": extension.name,
                    "outcome": "failed",
                    "code": error.code(),
                    "message": error.to_string(),
                }),
            },
        };
        outcomes.push(outcome);
        checkpoint = update_all_checkpoint(operation_id, "in_progress", &planned_items, &outcomes);
        installer
            .write_batch_result(operation_id, &checkpoint)
            .map_err(|error| format!("{}: {error}", error.code()))?;
    }
    ext_manager.refresh();
    checkpoint = update_all_checkpoint(operation_id, "completed", &planned_items, &outcomes);
    installer
        .write_batch_result(operation_id, &checkpoint)
        .map_err(|error| format!("{}: {error}", error.code()))?;
    Ok(checkpoint)
}

fn update_all_checkpoint(
    operation_id: &str,
    status: &str,
    planned_items: &[Value],
    outcomes: &[Value],
) -> Value {
    serde_json::json!({
        "action": "update-all",
        "items": outcomes,
        "operation_id": operation_id,
        "planned_items": planned_items,
        "schema_version": 1,
        "status": status,
        "summary": summarize_update_outcomes(outcomes),
    })
}

fn settings_serialize_error(error: serde_json::Error) -> SettingsError {
    SettingsError::new(
        "extension_setting_response_failed",
        format!("failed to serialize setting response: {error}"),
    )
}

fn summarize_update_outcomes(outcomes: &[Value]) -> Value {
    let mut summary = serde_json::Map::new();
    for key in [
        "updated",
        "unchanged",
        "skipped",
        "failed",
        "pending_consent",
    ] {
        let count = outcomes
            .iter()
            .filter(|outcome| outcome.get("outcome").and_then(Value::as_str) == Some(key))
            .count();
        summary.insert(key.to_string(), serde_json::json!(count));
    }
    Value::Object(summary)
}

fn extension_lifecycle_mutation_response(
    request_id: &str,
    result: &crate::extension::installer::ExtensionMutationResult,
    ext_manager: &ExtensionManager,
) -> OutputMessage {
    let mut data = serde_json::to_value(result).unwrap_or_else(|_| serde_json::json!({}));
    if let Value::Object(fields) = &mut data {
        if let Some(extension) = ext_manager
            .list()
            .iter()
            .find(|extension| extension.name == result.name)
        {
            fields.insert(
                "desired_state".to_string(),
                serde_json::json!(extension.desired_state),
            );
            fields.insert(
                "effective_state".to_string(),
                serde_json::json!("not_loaded"),
            );
            fields.insert("health".to_string(), serde_json::json!(extension.health));
        }
        let generation = crate::extension::state::load(None)
            .map(|loaded| loaded.state.active_generation)
            .unwrap_or_default();
        fields.insert("generation".to_string(), serde_json::json!(generation));
        fields.insert("warnings".to_string(), serde_json::json!([]));
    }
    registry_success(request_id, data)
}

fn extension_registry_value(extension: &crate::extension::Extension) -> Value {
    let update_status = match extension.source {
        crate::extension::ExtensionSourceKind::GitHttps => "unknown",
        crate::extension::ExtensionSourceKind::System => "not_updatable",
        crate::extension::ExtensionSourceKind::Conflict => "error",
        crate::extension::ExtensionSourceKind::PathCopy
        | crate::extension::ExtensionSourceKind::Link
        | crate::extension::ExtensionSourceKind::Legacy => "not_updatable",
    };
    let managed_metadata = extension.managed_install_metadata.as_ref();
    serde_json::json!({
        "activation": "next_session",
        "available_source_identities": extension.available_source_identities,
        "available_sources": extension.available_sources,
        "capabilities": extension.capabilities,
        "capability_fingerprint": extension.capability_fingerprint,
        "desired_state": extension.desired_state,
        "diagnostics": extension.diagnostics,
        "effective_state": "not_loaded",
        "health": extension.health,
        "is_active": false,
        "name": extension.name,
        "path": extension.path.to_string_lossy(),
        "settings": extension.settings,
        "contexts": extension.contexts.iter().map(|context| serde_json::json!({
            "id": context.id,
            "required": context.required,
        })).collect::<Vec<_>>(),
        "mcp_servers": extension.mcp_servers.iter().map(|server| serde_json::json!({
            "id": server.id,
            "required": server.required,
            "health": "next_session_validation",
        })).collect::<Vec<_>>(),
        "agent_directories": extension.agent_directories.iter()
            .map(|path| path.to_string_lossy())
            .collect::<Vec<_>>(),
        "schema_version": extension.schema_version,
        "source": extension.source,
        "source_identity": extension.source_identity,
        "content_digest": managed_metadata.map(|metadata| &metadata.content_digest),
        "installed_at": managed_metadata.map(|metadata| metadata.installed_at),
        "requested_ref": managed_metadata.and_then(|metadata| metadata.requested_ref.as_deref()),
        "resolved_revision": managed_metadata.and_then(|metadata| metadata.resolved_revision.as_deref()),
        "update_status": update_status,
        "updated_at": managed_metadata.map(|metadata| metadata.updated_at),
        "version": extension.version,
    })
}

fn extension_mutation_response(
    request_id: &str,
    action: &str,
    extension: &crate::extension::Extension,
) -> OutputMessage {
    OutputMessage::RegistryResponse {
        request_id: request_id.to_string(),
        success: true,
        data: Some(serde_json::json!({
            "action": action,
            "activation": "next_session",
            "desired_state": extension.desired_state,
            "diagnostics": extension.diagnostics,
            "effective_state": "not_loaded",
            "extension": extension.name,
            "health": extension.health,
            "warnings": [],
        })),
        error: None,
    }
}
