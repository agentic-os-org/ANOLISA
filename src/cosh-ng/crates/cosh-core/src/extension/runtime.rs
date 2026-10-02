//! Construction of complete immutable extension runtime snapshots.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use crate::config::CoreConfig;
use crate::skill::manager::expand_path;
use crate::skill::SkillManager;
use crate::tool::ToolRegistry;

use super::settings::ExtensionSettings;
use super::{
    AgentRegistry, DesiredState, ExtensionContextSnapshot, ExtensionDiagnostic, ExtensionHealth,
    ExtensionManager, McpRuntime, RuntimeGeneration, RuntimeSnapshot,
};

/// Inputs required to build one complete extension runtime generation.
pub struct RuntimeSnapshotBuilder<'a> {
    manager: &'a mut ExtensionManager,
    config: &'a CoreConfig,
    project_root: PathBuf,
    generation_id: u64,
    enable_shell_evidence_tool: bool,
    load_skills: bool,
    tool_selection: Option<&'a str>,
    settings: Option<&'a ExtensionSettings>,
}

impl<'a> RuntimeSnapshotBuilder<'a> {
    /// Creates a builder for an already refreshed extension catalog.
    pub fn new(
        manager: &'a mut ExtensionManager,
        config: &'a CoreConfig,
        project_root: PathBuf,
        generation_id: u64,
    ) -> Self {
        Self {
            manager,
            config,
            project_root,
            generation_id,
            enable_shell_evidence_tool: false,
            load_skills: true,
            tool_selection: None,
            settings: None,
        }
    }

    /// Includes the host-mediated shell evidence tool in the snapshot.
    pub fn with_shell_evidence(mut self, enabled: bool) -> Self {
        self.enable_shell_evidence_tool = enabled;
        self
    }

    /// Controls whether configured and extension-contributed skills are discovered.
    pub fn with_skill_loading(mut self, enabled: bool) -> Self {
        self.load_skills = enabled;
        self
    }

    /// Restricts every generated runtime snapshot to the CLI-selected tool set.
    pub fn with_tool_selection(mut self, selection: Option<&'a str>) -> Self {
        self.tool_selection = selection;
        self
    }

    /// Uses a staged settings view while validating this candidate generation.
    pub fn with_settings(mut self, settings: &'a ExtensionSettings) -> Self {
        self.settings = Some(settings);
        self
    }

    /// Validates every contribution and starts generation-owned MCP servers.
    pub async fn build(self) -> RuntimeSnapshot {
        let mut diagnostics = self.manager.catalog_diagnostics().to_vec();

        let owned_settings = self
            .settings
            .is_none()
            .then(|| ExtensionSettings::new(self.manager.workspace_dir().to_path_buf()));
        let (settings, settings_error) = match (self.settings, owned_settings.as_ref()) {
            (Some(settings), _) => (Some(settings), None),
            (None, Some(Ok(settings))) => (Some(settings), None),
            (None, Some(Err(error))) => (None, Some(error.to_string())),
            (None, None) => unreachable!("owned settings are created when none are injected"),
        };

        let context = ExtensionContextSnapshot::build_with_settings(
            self.manager,
            settings.ok_or_else(|| settings_error.clone().unwrap_or_default()),
        );
        diagnostics.extend(context.diagnostics().iter().cloned());

        let mcp = Arc::new(
            McpRuntime::start_with_settings(
                self.manager,
                settings.ok_or_else(|| settings_error.clone().unwrap_or_default()),
            )
            .await,
        );
        diagnostics.extend(mcp.diagnostics().iter().cloned());

        let custom_paths = self
            .config
            .skills
            .custom_paths
            .iter()
            .filter_map(|path| expand_path(path))
            .collect::<Vec<_>>();
        let skill_manager = SkillManager::new(
            self.project_root.clone(),
            custom_paths,
            self.manager.skill_dirs(),
        );
        if self.load_skills {
            skill_manager.refresh().await;
        }
        let skills = skill_manager.list().await;

        let mut tools = ToolRegistry::with_defaults(skill_manager);
        let mut tool_registration_healthy = true;
        crate::tool::mcp::register_configured_tools(
            &mut tools,
            &self.config.mcp.servers,
            &self.project_root,
        )
        .await;
        if let Err(error) = mcp.register_tools(&mut tools) {
            tool_registration_healthy = false;
            diagnostics.push(ExtensionDiagnostic::new(error.code(), error.to_string()));
        }
        if self.enable_shell_evidence_tool {
            tools = tools.with_shell_evidence();
        }
        if let Some(selection) = self.tool_selection {
            if let Err(error) = tools.retain_selected_tools(selection) {
                tool_registration_healthy = false;
                diagnostics.push(ExtensionDiagnostic::new("tool_selection_invalid", error));
            }
        }
        let tools = Arc::new(tools);

        let allowed_tools = tools.names().into_iter().collect::<BTreeSet<_>>();
        let workspace_trusted = settings
            .map(ExtensionSettings::workspace_trusted)
            .unwrap_or(false);
        let agents = AgentRegistry::build(
            self.manager,
            &allowed_tools,
            workspace_trusted,
            self.config.agent.approval_mode,
        );
        diagnostics.extend(agents.diagnostics().iter().cloned());

        let active_extensions = self
            .manager
            .list()
            .iter()
            .filter(|extension| extension.is_active)
            .map(|extension| extension.name.clone())
            .collect::<BTreeSet<_>>();
        let extension_health = self
            .manager
            .list()
            .iter()
            .map(|extension| (extension.name.clone(), extension.health))
            .collect();

        let fingerprint = match self.manager.runtime_fingerprint() {
            Ok(fingerprint) => fingerprint,
            Err(diagnostic) => {
                diagnostics.push(diagnostic);
                "runtime-fingerprint-unavailable".to_string()
            }
        };
        let required_contributions_healthy = !self.manager.list().iter().any(|extension| {
            extension.desired_state == DesiredState::Enabled
                && matches!(
                    extension.health,
                    ExtensionHealth::Broken | ExtensionHealth::Conflict
                )
        });
        let fingerprint_healthy = fingerprint != "runtime-fingerprint-unavailable";
        let generation = RuntimeGeneration {
            id: self.generation_id,
            fingerprint,
            healthy: required_contributions_healthy
                && tool_registration_healthy
                && fingerprint_healthy,
            stale: false,
        };

        RuntimeSnapshot::new(
            generation,
            skills,
            active_extensions,
            extension_health,
            tools,
            self.manager.hook_definitions(),
            context,
            mcp,
            agents,
            diagnostics,
        )
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::config::McpServerConfig;
    use crate::extension::EXTENSION_CONFIG_FILENAME;

    #[tokio::test]
    async fn snapshot_includes_configured_mcp_tools() {
        let temporary = tempfile::tempdir().unwrap();
        let server = temporary.path().join("configured-mcp.sh");
        fs::write(
            &server,
            r#"#!/bin/sh
while IFS= read -r line; do
  case "$line" in
    *'"method":"initialize"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"configured","version":"1.0"}}}'
      ;;
    *'"method":"tools/list"'*)
      printf '%s\n' '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echo","inputSchema":{"type":"object"}}]}}'
      ;;
  esac
done
"#,
        )
        .unwrap();
        let mut config = CoreConfig::default();
        config.mcp.servers.insert(
            "configured".to_string(),
            McpServerConfig {
                command: "sh".to_string(),
                url: None,
                args: vec![server.to_string_lossy().into_owned()],
                env: std::collections::HashMap::new(),
                bearer_token: None,
                oauth: Default::default(),
                timeout_ms: 1_000,
                startup_timeout_ms: 1_000,
                allowed_tools: None,
            },
        );
        let mut manager = ExtensionManager::new_isolated_with_state(
            temporary.path().join("workspace"),
            Some(temporary.path().join("user")),
            Some(temporary.path().join("system")),
            temporary.path().join("states"),
        );
        manager.refresh();

        let snapshot = RuntimeSnapshotBuilder::new(
            &mut manager,
            &config,
            temporary.path().join("workspace"),
            1,
        )
        .build()
        .await;

        let tool = snapshot
            .tools
            .get("mcp__configured__echo")
            .expect("configured MCP tool");
        assert_eq!(tool.kind(), crate::tool::ToolKind::Mcp);
    }

    #[tokio::test]
    async fn snapshot_applies_cli_tool_selection() {
        let temporary = tempfile::tempdir().unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            temporary.path().join("workspace"),
            Some(temporary.path().join("user")),
            Some(temporary.path().join("system")),
            temporary.path().join("states"),
        );
        manager.refresh();

        let snapshot = RuntimeSnapshotBuilder::new(
            &mut manager,
            &CoreConfig::default(),
            temporary.path().join("workspace"),
            1,
        )
        .with_tool_selection(Some("read_file"))
        .build()
        .await;

        assert!(snapshot.generation.healthy);
        assert_eq!(snapshot.tools.names(), ["read_file"]);
    }

    #[tokio::test]
    async fn snapshot_captures_all_contribution_owners() {
        let temporary = tempfile::tempdir().unwrap();
        let user = temporary.path().join("extensions");
        let package = user.join("example.ops");
        fs::create_dir_all(package.join("skills/inspect")).unwrap();
        fs::create_dir_all(package.join("agents")).unwrap();
        fs::write(
            package.join("skills/inspect/SKILL.md"),
            "---\nname: inspect\ndescription: Inspect safely\n---\nUse read_file.\n",
        )
        .unwrap();
        fs::write(package.join("context.md"), "EXTENSION CONTEXT").unwrap();
        fs::write(
            package.join("agents/reviewer.md"),
            "---\nname: reviewer\ntools: [read_file]\nskills: [inspect]\n---\nReview evidence.\n",
        )
        .unwrap();
        fs::write(
            package.join(EXTENSION_CONFIG_FILENAME),
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 1,
                "name": "example.ops",
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"},
                "skills": ["skills"],
                "hooks": {
                    "BeforeModel": [{
                        "hooks": [{
                            "type": "command",
                            "name": "observe",
                            "command": "/usr/bin/true"
                        }]
                    }]
                },
                "contextFiles": [{"id": "runbook", "path": "context.md", "required": true}],
                "agents": ["agents"]
            }))
            .unwrap(),
        )
        .unwrap();

        let mut manager = ExtensionManager::new_isolated_with_state(
            temporary.path().join("workspace"),
            Some(user),
            Some(temporary.path().join("system")),
            temporary.path().join("states"),
        );
        manager.refresh();
        let snapshot = RuntimeSnapshotBuilder::new(
            &mut manager,
            &CoreConfig::default(),
            temporary.path().join("workspace"),
            7,
        )
        .build()
        .await;

        assert!(snapshot.generation.healthy);
        assert_eq!(snapshot.generation.id, 7);
        assert!(snapshot.skills.iter().any(|skill| skill.name == "inspect"));
        assert!(!snapshot.hooks.is_empty());
        assert!(snapshot
            .context
            .rendered()
            .is_some_and(|context| context.contains("EXTENSION CONTEXT")));
        assert_eq!(snapshot.agents.list().len(), 1);
        assert!(snapshot.mcp.statuses().is_empty());
        assert_eq!(
            snapshot.extension_health.get("example.ops"),
            Some(&ExtensionHealth::Degraded)
        );
    }

    #[tokio::test]
    async fn required_failure_marks_candidate_unhealthy() {
        let temporary = tempfile::tempdir().unwrap();
        let user = temporary.path().join("extensions");
        let package = user.join("example.ops");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join(EXTENSION_CONFIG_FILENAME),
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 1,
                "name": "example.ops",
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"},
                "contextFiles": [{"id": "missing", "path": "missing.md", "required": true}]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            temporary.path().join("workspace"),
            Some(user),
            Some(temporary.path().join("system")),
            temporary.path().join("states"),
        );
        manager.refresh();
        let snapshot = RuntimeSnapshotBuilder::new(
            &mut manager,
            &CoreConfig::default(),
            temporary.path().join("workspace"),
            2,
        )
        .build()
        .await;

        assert!(!snapshot.generation.healthy);
        assert!(snapshot
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "extension_context_unreadable"));
        assert_eq!(
            snapshot.extension_health.get("example.ops"),
            Some(&ExtensionHealth::Broken)
        );
    }

    #[tokio::test]
    async fn optional_failure_is_preserved_in_snapshot_health() {
        let temporary = tempfile::tempdir().unwrap();
        let user = temporary.path().join("extensions");
        let package = user.join("example.ops");
        fs::create_dir_all(&package).unwrap();
        fs::write(
            package.join(EXTENSION_CONFIG_FILENAME),
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 1,
                "name": "example.ops",
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"},
                "contextFiles": [{"id": "missing", "path": "missing.md", "required": false}]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            temporary.path().join("workspace"),
            Some(user),
            Some(temporary.path().join("system")),
            temporary.path().join("states"),
        );
        manager.refresh();
        let snapshot = RuntimeSnapshotBuilder::new(
            &mut manager,
            &CoreConfig::default(),
            temporary.path().join("workspace"),
            3,
        )
        .build()
        .await;

        assert!(snapshot.generation.healthy);
        assert_eq!(
            snapshot.extension_health.get("example.ops"),
            Some(&ExtensionHealth::Degraded)
        );
    }
}
