use std::io::{self, BufRead, Write};

use serde_json::Value;

use crate::cli::CliArgs;
use crate::config::CoreConfig;
use crate::extension::config::flatten_hook_groups;
use crate::extension::generation::ReloadOutcome;
use crate::extension::installer::ExtensionInstaller;
use crate::extension::scaffold::{scaffold_extension, ExtensionTemplate};
use crate::extension::settings::{ExtensionSettings, SettingScope, SettingsError};
use crate::extension::{
    ExtensionManager, GenerationController, McpRuntime, RuntimeSnapshot, RuntimeSnapshotBuilder,
};
use crate::protocol::{InputMessage, OutputMessage};
use crate::skill::manager::expand_path;
use crate::skill::SkillManager;

mod auth;
mod extensions;

use auth::handle_auth;
use extensions::handle_extensions;

/// Bridge from registry mutations into the generation controller of a live core process.
#[derive(Clone)]
pub(crate) struct LiveExtensionRuntime {
    controller: GenerationController,
    enable_shell_evidence_tool: bool,
    load_skills: bool,
    tool_selection: Option<String>,
    state_dir_override: Option<std::path::PathBuf>,
    settings_override: Option<std::sync::Arc<ExtensionSettings>>,
}

#[derive(Debug, Clone)]
struct RuntimePublication {
    activation: &'static str,
    candidate_generation: u64,
    current_generation: u64,
    active_runs: usize,
    pending: bool,
}

impl LiveExtensionRuntime {
    pub(crate) fn new(
        controller: GenerationController,
        enable_shell_evidence_tool: bool,
        load_skills: bool,
        tool_selection: Option<String>,
    ) -> Self {
        Self {
            controller,
            enable_shell_evidence_tool,
            load_skills,
            tool_selection,
            state_dir_override: None,
            settings_override: None,
        }
    }

    #[cfg(test)]
    fn new_isolated(
        controller: GenerationController,
        enable_shell_evidence_tool: bool,
        state_dir: std::path::PathBuf,
        settings: std::sync::Arc<ExtensionSettings>,
    ) -> Self {
        Self {
            controller,
            enable_shell_evidence_tool,
            load_skills: true,
            tool_selection: None,
            state_dir_override: Some(state_dir),
            settings_override: Some(settings),
        }
    }

    fn next_generation(&self) -> u64 {
        self.controller.status().current.saturating_add(1).max(1)
    }

    fn configure_builder<'a>(
        &'a self,
        builder: RuntimeSnapshotBuilder<'a>,
    ) -> RuntimeSnapshotBuilder<'a> {
        builder
            .with_shell_evidence(self.enable_shell_evidence_tool)
            .with_skill_loading(self.load_skills)
            .with_tool_selection(self.tool_selection.as_deref())
    }

    async fn publish(&self, snapshot: RuntimeSnapshot) -> Result<RuntimePublication, String> {
        let candidate_generation = snapshot.generation.id;
        if let Some(previous) = self.controller.stage(snapshot) {
            previous.mcp.shutdown().await;
        }
        let outcome = self.controller.reload();
        if matches!(
            outcome,
            ReloadOutcome::NoCandidate
                | ReloadOutcome::CandidateUnhealthy
                | ReloadOutcome::CandidateStale
        ) {
            if let Some(candidate) = self.controller.discard_candidate() {
                candidate.mcp.shutdown().await;
            }
            return Err(format!(
                "extension_runtime_publish_failed: reload outcome was {outcome:?}"
            ));
        }
        let status = self.controller.status();
        if outcome == ReloadOutcome::Activated {
            crate::extension::state::persist_active_generation(
                status.current,
                self.state_dir_override.as_deref(),
            )
            .map_err(|error| format!("{}: {error}", error.code()))?;
        }
        Ok(RuntimePublication {
            activation: if outcome == ReloadOutcome::Activated {
                "immediate"
            } else {
                "pending_safe_reload"
            },
            candidate_generation,
            current_generation: status.current,
            active_runs: status.active_runs,
            pending: status.pending,
        })
    }

    pub(crate) fn persist_current_generation(&self) -> Result<u64, String> {
        let current = self.controller.status().current;
        crate::extension::state::persist_active_generation(
            current,
            self.state_dir_override.as_deref(),
        )
        .map_err(|error| format!("{}: {error}", error.code()))
    }

    pub(crate) async fn refresh_linked_runtime(
        &self,
        config: &CoreConfig,
        manager: &mut ExtensionManager,
    ) -> Result<bool, String> {
        manager.refresh();
        let linked_change = manager.list().iter().any(|extension| {
            extension.source == crate::extension::ExtensionSourceKind::Link
                && extension.diagnostics.iter().any(|diagnostic| {
                    matches!(
                        diagnostic.code.as_str(),
                        "extension_link_stale" | "extension_link_consent_stale"
                    )
                })
        });
        if !linked_change {
            return Ok(false);
        }
        let current_fingerprint = self.controller.current().generation.fingerprint.clone();
        let observed_fingerprint = manager
            .runtime_fingerprint()
            .map_err(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))?;
        if observed_fingerprint == current_fingerprint {
            return Ok(false);
        }

        let generation = self.next_generation();
        let workspace = manager.workspace_dir().to_path_buf();
        let mut builder = self.configure_builder(RuntimeSnapshotBuilder::new(
            manager, config, workspace, generation,
        ));
        if let Some(settings) = self.settings_override.as_deref() {
            builder = builder.with_settings(settings);
        }
        let mut snapshot = builder.build().await;
        manager.refresh();
        let final_fingerprint = manager
            .runtime_fingerprint()
            .map_err(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.message))?;
        if final_fingerprint != snapshot.generation.fingerprint {
            snapshot.generation.stale = true;
        }
        if !snapshot.generation.healthy {
            let diagnostics = snapshot
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code.as_str())
                .collect::<Vec<_>>()
                .join(",");
            snapshot.mcp.shutdown().await;
            return Err(format!(
                "extension_link_candidate_unhealthy: generation {generation} failed: {diagnostics}"
            ));
        }
        self.publish(snapshot).await.map(|_| true)
    }

    fn projection(&self, extension: Option<&str>) -> Value {
        let status = self.controller.status();
        let snapshot = self.controller.current();
        let prefix = extension.map(|name| format!("{name}/"));
        let mcp_servers = snapshot
            .mcp
            .statuses()
            .iter()
            .filter(|server| {
                prefix
                    .as_ref()
                    .is_none_or(|prefix| server.id.starts_with(prefix))
            })
            .collect::<Vec<_>>();
        let agents = snapshot
            .agents
            .list()
            .iter()
            .filter(|agent| {
                prefix
                    .as_ref()
                    .is_none_or(|prefix| agent.id.starts_with(prefix))
            })
            .collect::<Vec<_>>();
        let is_active = extension
            .map(|name| snapshot.active_extensions.contains(name))
            .unwrap_or(true);
        let health = extension.and_then(|name| snapshot.extension_health.get(name));
        serde_json::json!({
            "active_runs": status.active_runs,
            "agents": agents,
            "candidate_generation": status.candidate,
            "diagnostics": snapshot.diagnostics,
            "effective_state": if is_active { "enabled" } else { "disabled" },
            "fingerprint": snapshot.generation.fingerprint,
            "generation": status.current,
            "health": health,
            "healthy": snapshot.generation.healthy,
            "is_active": is_active,
            "mcp_servers": mcp_servers,
            "pending": status.pending,
            "stale": snapshot.generation.stale,
        })
    }
}

pub async fn run(args: &CliArgs, mut config: CoreConfig) {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut writer = io::BufWriter::new(stdout.lock());

    // --- Extension Manager setup (no LLM/provider init) ---
    // Prefer the workspace explicitly passed from the caller so the registry
    // resolves the same project root as the interactive/headless runtime.
    let project_root = args.workspace_root();
    let mut ext_manager = ExtensionManager::new(project_root.clone());
    if !args.bare {
        ext_manager.refresh();
    }

    // --- Skill Manager setup ---
    let custom_paths: Vec<std::path::PathBuf> = config
        .skills
        .custom_paths
        .iter()
        .filter_map(|p| expand_path(p))
        .collect();
    let skill_manager = SkillManager::new(project_root, custom_paths, ext_manager.skill_dirs());
    if !args.bare {
        skill_manager.refresh().await;
    }

    // Read one line from stdin
    let line = {
        let mut buf = String::new();
        match stdin.lock().read_line(&mut buf) {
            Ok(0) => return, // EOF
            Ok(_) => buf,
            Err(_) => return,
        }
    };

    let msg: InputMessage = match serde_json::from_str(line.trim()) {
        Ok(m) => m,
        Err(e) => {
            tracing::debug!("failed to parse input: {e}");
            return;
        }
    };

    match msg {
        InputMessage::RegistryRequest {
            request_id,
            domain,
            action,
            params,
        } => {
            let response = handle_registry_request(
                &request_id,
                &domain,
                &action,
                &params,
                &mut config,
                &mut ext_manager,
                Some(&skill_manager),
                None,
            )
            .await;
            emit(&mut writer, &response);
        }
        _ => {
            tracing::debug!("expected registry_request, got other message type");
        }
    }
}

pub(crate) async fn handle_registry_request(
    request_id: &str,
    domain: &str,
    action: &str,
    params: &Value,
    config: &mut CoreConfig,
    ext_manager: &mut ExtensionManager,
    skill_manager: Option<&SkillManager>,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> OutputMessage {
    match domain {
        "auth" => handle_auth(request_id, action, params, config).await,
        "extensions" => {
            handle_extensions(
                request_id,
                action,
                params,
                config,
                ext_manager,
                live_runtime,
            )
            .await
        }
        "skills" => match registry_skills(skill_manager, live_runtime).await {
            Ok(skills) => handle_skills(request_id, action, params, &skills),
            Err(error) => registry_error(request_id, error),
        },
        "hooks" => handle_hooks(request_id, action, params, ext_manager),
        _ => OutputMessage::RegistryResponse {
            request_id: request_id.to_string(),
            success: false,
            data: None,
            error: Some(format!("unknown domain: {domain}")),
        },
    }
}

fn registry_error(request_id: &str, error: &str) -> OutputMessage {
    OutputMessage::RegistryResponse {
        request_id: request_id.to_string(),
        success: false,
        data: None,
        error: Some(error.to_string()),
    }
}

#[cfg(all(test, unix))]
mod link_runtime_tests {
    use std::fs;
    use std::sync::Arc;

    use super::*;
    use crate::extension::settings::KeyringSecretBackend;

    #[tokio::test]
    async fn linked_content_change_reloads_at_safe_point() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let user = temporary.path().join("extensions");
        let system = temporary.path().join("system");
        let states = temporary.path().join("states");
        let workspace = temporary.path().join("workspace");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&system).unwrap();
        fs::create_dir_all(&workspace).unwrap();
        fs::write(
            source.join(crate::extension::EXTENSION_CONFIG_FILENAME),
            r#"{
                "schemaVersion": 1,
                "name": "example.dev",
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"}
            }"#,
        )
        .unwrap();
        fs::write(source.join("README.md"), "one").unwrap();
        let installer = ExtensionInstaller::new(user.clone(), states.clone());
        let preflight = installer.preflight_link(&source).unwrap();
        installer
            .commit(&preflight.operation_id, &preflight.capability_fingerprint)
            .unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            workspace.clone(),
            Some(user),
            Some(system),
            states.clone(),
        );
        manager.refresh();
        let settings = Arc::new(ExtensionSettings::new_isolated(
            temporary.path().join("user-settings"),
            temporary.path().join("workspace-settings"),
            true,
            Arc::new(KeyringSecretBackend),
        ));
        let initial =
            RuntimeSnapshotBuilder::new(&mut manager, &CoreConfig::default(), workspace, 1)
                .with_settings(&settings)
                .build()
                .await;
        let runtime = LiveExtensionRuntime::new_isolated(
            GenerationController::new(initial),
            false,
            states,
            settings,
        );

        fs::write(source.join("README.md"), "two").unwrap();
        assert!(runtime
            .refresh_linked_runtime(&CoreConfig::default(), &mut manager)
            .await
            .unwrap());
        assert_eq!(runtime.controller.status().current, 2);
        assert!(runtime
            .controller
            .current()
            .active_extensions
            .contains("example.dev"));
    }
}

async fn registry_skills(
    skill_manager: Option<&SkillManager>,
    live_runtime: Option<&LiveExtensionRuntime>,
) -> Result<Vec<crate::skill::SkillConfig>, &'static str> {
    if let Some(runtime) = live_runtime {
        return Ok(runtime.controller.current().skills.clone());
    }
    match skill_manager {
        Some(manager) => Ok(manager.list().await),
        None => Err("skill registry is unavailable"),
    }
}

fn handle_skills(
    request_id: &str,
    action: &str,
    params: &Value,
    skills: &[crate::skill::SkillConfig],
) -> OutputMessage {
    match action {
        "list" => {
            let disabled = crate::state::load_disabled(crate::state::SKILLS_STATE);
            let skills: Vec<Value> = skills
                .iter()
                .map(|s| {
                    let is_disabled = disabled.contains(&s.name);
                    serde_json::json!({
                        "name": s.name,
                        "description": s.description,
                        "level": s.level.to_string(),
                        "disabled": is_disabled,
                    })
                })
                .collect();
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(Value::Array(skills)),
                error: None,
            }
        }
        "detail" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            match skills.iter().find(|skill| skill.name == name) {
                Some(skill) => {
                    let disabled = crate::state::load_disabled(crate::state::SKILLS_STATE);
                    let is_disabled = disabled.contains(&skill.name);
                    let detail = serde_json::json!({
                        "name": skill.name,
                        "description": skill.description,
                        "level": skill.level.to_string(),
                        "base_dir": skill.base_dir.to_string_lossy(),
                        "disabled": is_disabled,
                    });
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
                    error: Some(format!("skill not found: {name}")),
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
            // Validate skill exists
            if !skills.iter().any(|skill| skill.name == name) {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some(format!("skill not found: {name}")),
                };
            }
            if let Err(e) = crate::state::remove_disabled(crate::state::SKILLS_STATE, name) {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some(format!("failed to enable skill: {e}")),
                };
            }
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(serde_json::json!({ "enabled": name })),
                error: None,
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
            // Validate skill exists
            if !skills.iter().any(|skill| skill.name == name) {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some(format!("skill not found: {name}")),
                };
            }
            if let Err(e) = crate::state::add_disabled(crate::state::SKILLS_STATE, name) {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some(format!("failed to disable skill: {e}")),
                };
            }
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(serde_json::json!({ "disabled": name })),
                error: None,
            }
        }
        _ => OutputMessage::RegistryResponse {
            request_id: request_id.to_string(),
            success: false,
            data: None,
            error: Some(format!("unsupported action for skills: {action}")),
        },
    }
}

fn handle_hooks(
    request_id: &str,
    action: &str,
    params: &Value,
    ext_manager: &ExtensionManager,
) -> OutputMessage {
    match action {
        "list" => {
            let disabled = crate::state::load_disabled(crate::state::HOOKS_STATE);
            let mut hooks_list: Vec<Value> = Vec::new();
            for ext in ext_manager.list() {
                if !ext.is_active || ext.config.hooks.is_empty() {
                    continue;
                }
                // Collect all hook events for this extension
                let events = [
                    ("PreToolUse", &ext.config.hooks.pre_tool_use),
                    ("PostToolUse", &ext.config.hooks.post_tool_use),
                    (
                        "PostToolUseFailure",
                        &ext.config.hooks.post_tool_use_failure,
                    ),
                    ("UserPromptSubmit", &ext.config.hooks.user_prompt_submit),
                    ("SessionStart", &ext.config.hooks.session_start),
                    ("Stop", &ext.config.hooks.stop),
                    ("BeforeModel", &ext.config.hooks.before_model),
                    ("AfterModel", &ext.config.hooks.after_model),
                ];
                for (event_name, groups) in events {
                    for hook_def in flatten_hook_groups(groups) {
                        let name = hook_def.name.as_deref().unwrap_or(&hook_def.command);
                        let is_disabled = disabled.contains(name);
                        hooks_list.push(serde_json::json!({
                            "name": name,
                            "event": event_name,
                            "extension": ext.name,
                            "command": hook_def.command,
                            "matcher": hook_def.matcher,
                            "disabled": is_disabled,
                        }));
                    }
                }
            }
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(Value::Array(hooks_list)),
                error: None,
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
            // Validate hook exists in known extensions
            let known = collect_all_hook_names(ext_manager);
            if !known.contains(name) {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some(format!("unknown hook: {name}")),
                };
            }
            if let Err(e) = crate::state::remove_disabled(crate::state::HOOKS_STATE, name) {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some(format!("failed to enable hook: {e}")),
                };
            }
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(serde_json::json!({ "enabled": name })),
                error: None,
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
            // Validate hook exists in known extensions
            let known = collect_all_hook_names(ext_manager);
            if !known.contains(name) {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some(format!("unknown hook: {name}")),
                };
            }
            if let Err(e) = crate::state::add_disabled(crate::state::HOOKS_STATE, name) {
                return OutputMessage::RegistryResponse {
                    request_id: request_id.to_string(),
                    success: false,
                    data: None,
                    error: Some(format!("failed to disable hook: {e}")),
                };
            }
            OutputMessage::RegistryResponse {
                request_id: request_id.to_string(),
                success: true,
                data: Some(serde_json::json!({ "disabled": name })),
                error: None,
            }
        }
        _ => OutputMessage::RegistryResponse {
            request_id: request_id.to_string(),
            success: false,
            data: None,
            error: Some(format!("unsupported action for hooks: {action}")),
        },
    }
}

fn collect_all_hook_names(ext_manager: &ExtensionManager) -> std::collections::HashSet<String> {
    let mut names = std::collections::HashSet::new();
    for ext in ext_manager.list() {
        let events = [
            &ext.config.hooks.pre_tool_use,
            &ext.config.hooks.post_tool_use,
            &ext.config.hooks.post_tool_use_failure,
            &ext.config.hooks.user_prompt_submit,
            &ext.config.hooks.session_start,
            &ext.config.hooks.stop,
            &ext.config.hooks.before_model,
            &ext.config.hooks.after_model,
        ];
        for groups in events {
            for def in flatten_hook_groups(groups) {
                if let Some(name) = def.name {
                    names.insert(name);
                }
            }
        }
    }
    names
}

fn emit<W: Write>(writer: &mut W, msg: &OutputMessage) {
    if let Ok(json) = serde_json::to_string(msg) {
        let _ = writeln!(writer, "{json}");
        let _ = writer.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{AiConfig, ProviderConfig};
    use crate::extension::generation::ReloadOutcome;
    use crate::extension::RuntimeGeneration;
    use crate::skill::{SkillConfig, SkillLevel};
    use crate::tool::ToolRegistry;
    use std::path::PathBuf;
    use std::sync::Arc;

    fn runtime_skill(name: &str) -> SkillConfig {
        SkillConfig {
            name: name.to_string(),
            description: format!("{name} description"),
            allowed_tools: Vec::new(),
            body: format!("{name} body"),
            level: SkillLevel::Extension,
            file_path: PathBuf::from(format!("/{name}/SKILL.md")),
            base_dir: PathBuf::from(format!("/{name}")),
        }
    }

    fn runtime_snapshot(id: u64, names: &[&str], healthy: bool) -> RuntimeSnapshot {
        let mut snapshot = RuntimeSnapshot::bootstrap(
            RuntimeGeneration::healthy(id, format!("generation-{id}")),
            Arc::new(ToolRegistry::new()),
        );
        snapshot.skills = names.iter().map(|name| runtime_skill(name)).collect();
        snapshot.generation.healthy = healthy;
        snapshot
    }

    async fn live_registry_skill_names(
        runtime: &LiveExtensionRuntime,
        config: &mut CoreConfig,
        manager: &mut ExtensionManager,
    ) -> Vec<String> {
        let response = handle_registry_request(
            "skills-list",
            "skills",
            "list",
            &Value::Null,
            config,
            manager,
            None,
            Some(runtime),
        )
        .await;
        let OutputMessage::RegistryResponse {
            success: true,
            data: Some(Value::Array(skills)),
            ..
        } = response
        else {
            panic!("unexpected response: {response:?}");
        };
        skills
            .iter()
            .filter_map(|skill| skill.get("name").and_then(Value::as_str))
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn live_skill_registry_follows_effective_generation() {
        let workspace = tempfile::tempdir().unwrap();
        let mut manager = ExtensionManager::new(workspace.path().to_path_buf());
        let mut config = CoreConfig::default();
        let controller = GenerationController::new(runtime_snapshot(
            1,
            &["disabled-skill", "uninstalled-skill"],
            true,
        ));
        let runtime = LiveExtensionRuntime::new(controller.clone(), false, true, None);

        assert_eq!(
            live_registry_skill_names(&runtime, &mut config, &mut manager).await,
            ["disabled-skill", "uninstalled-skill"]
        );

        controller.stage(runtime_snapshot(2, &["uninstalled-skill"], true));
        assert_eq!(controller.reload(), ReloadOutcome::Activated);
        assert_eq!(
            live_registry_skill_names(&runtime, &mut config, &mut manager).await,
            ["uninstalled-skill"]
        );

        controller.stage(runtime_snapshot(3, &[], true));
        assert_eq!(controller.reload(), ReloadOutcome::Activated);
        assert!(
            live_registry_skill_names(&runtime, &mut config, &mut manager)
                .await
                .is_empty()
        );

        controller.stage(runtime_snapshot(4, &["failed-candidate-skill"], false));
        assert_eq!(controller.reload(), ReloadOutcome::CandidateUnhealthy);
        assert!(
            live_registry_skill_names(&runtime, &mut config, &mut manager)
                .await
                .is_empty()
        );
    }

    #[tokio::test]
    async fn auth_state_marks_system_providers_as_not_editable() {
        let mut config = CoreConfig::default();
        config.ai.active_provider = Some("system-provider".to_string());
        config.ai.providers.insert(
            "system-provider".to_string(),
            ProviderConfig {
                provider_type: Some("dashscope".to_string()),
                api_key: Some("sk-system".to_string()),
                model: Some("system-model".to_string()),
                ..Default::default()
            },
        );
        config.system_ai = AiConfig {
            providers: config.ai.providers.clone(),
            ..Default::default()
        };
        config.user_ai.providers.insert(
            "user-provider".to_string(),
            ProviderConfig {
                provider_type: Some("dashscope".to_string()),
                api_key: Some("sk-user".to_string()),
                ..Default::default()
            },
        );
        config.ai.providers.extend(config.user_ai.providers.clone());

        let response = handle_auth("test-1", "state", &Value::Null, &mut config).await;
        let OutputMessage::RegistryResponse {
            success: true,
            data: Some(data),
            ..
        } = response
        else {
            panic!("unexpected response: {response:?}");
        };
        let saved = data["saved_providers"].as_array().unwrap();
        let system = saved
            .iter()
            .find(|provider| provider["provider_id"] == "system-provider")
            .unwrap();
        let user = saved
            .iter()
            .find(|provider| provider["provider_id"] == "user-provider")
            .unwrap();

        assert_eq!(system["source"], "system");
        assert_eq!(system["editable"], false);
        assert_eq!(user["source"], "user");
        assert_eq!(user["editable"], true);
    }

    #[tokio::test]
    async fn auth_configure_rejects_invalid_provider_id() {
        let mut config = CoreConfig::default();
        let response = handle_auth(
            "test-1",
            "configure",
            &serde_json::json!({
                "provider_id": "bad.provider",
                "provider_type": "dashscope",
                "values": {
                    "api_key": "sk-user"
                }
            }),
            &mut config,
        )
        .await;

        let OutputMessage::RegistryResponse { success, error, .. } = response else {
            panic!("unexpected response: {response:?}");
        };
        assert!(!success);
        assert!(error.unwrap().contains("invalid provider_id"));
        assert!(config.user_ai.providers.is_empty());
    }

    #[tokio::test]
    async fn auth_configure_rejects_invalid_base_url_without_mutating_config() {
        let mut config = CoreConfig::default();
        let response = handle_auth(
            "test-1",
            "configure",
            &serde_json::json!({
                "provider_id": "bad-url",
                "provider_type": "openai_compat",
                "values": {
                    "base_url": "error-testhttps://api.example.com/v1",
                    "api_key": "sk-user",
                    "model": "qwen-test"
                }
            }),
            &mut config,
        )
        .await;

        let OutputMessage::RegistryResponse { success, error, .. } = response else {
            panic!("unexpected response: {response:?}");
        };
        assert!(!success);
        assert!(error.unwrap().contains("invalid base_url"));
        assert!(config.ai.providers.is_empty());
        assert!(config.user_ai.providers.is_empty());
    }

    #[tokio::test]
    async fn auth_configure_rejects_system_provider_overwrite() {
        let mut config = CoreConfig::default();
        config.ai.providers.insert(
            "system-provider".to_string(),
            ProviderConfig {
                provider_type: Some("dashscope".to_string()),
                api_key: Some("sk-system".to_string()),
                ..Default::default()
            },
        );
        config.system_ai = AiConfig {
            providers: config.ai.providers.clone(),
            ..Default::default()
        };

        let response = handle_auth(
            "test-1",
            "configure",
            &serde_json::json!({
                "provider_id": "system-provider",
                "provider_type": "dashscope",
                "values": {
                    "api_key": "•••••••••"
                }
            }),
            &mut config,
        )
        .await;

        let OutputMessage::RegistryResponse { success, error, .. } = response else {
            panic!("unexpected response: {response:?}");
        };
        assert!(!success);
        assert!(error.unwrap().contains("not editable"));
        assert!(!config.user_ai.providers.contains_key("system-provider"));
    }

    #[tokio::test]
    async fn auth_delete_rejects_system_provider() {
        let mut config = CoreConfig::default();
        config.ai.providers.insert(
            "system-provider".to_string(),
            ProviderConfig {
                provider_type: Some("dashscope".to_string()),
                api_key: Some("sk-system".to_string()),
                ..Default::default()
            },
        );
        config.system_ai = AiConfig {
            providers: config.ai.providers.clone(),
            ..Default::default()
        };

        let response = handle_auth(
            "test-1",
            "delete",
            &serde_json::json!({ "provider_id": "system-provider" }),
            &mut config,
        )
        .await;

        let OutputMessage::RegistryResponse { success, error, .. } = response else {
            panic!("unexpected response: {response:?}");
        };
        assert!(!success);
        assert!(error.unwrap().contains("not removable"));
        assert!(config.ai.providers.contains_key("system-provider"));
    }
}
