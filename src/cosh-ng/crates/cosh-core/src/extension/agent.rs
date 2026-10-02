//! Strict extension agent discovery and non-escalating capability resolution.

use std::collections::BTreeSet;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config::ApprovalMode;

use super::identity::{validate_local_id, CapabilityId, CapabilityKind};
use super::{Extension, ExtensionDiagnostic, ExtensionManager};

const MAX_AGENT_FILE_BYTES: usize = 64 * 1024;

/// Strict parsed agent contribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedAgent {
    /// Canonical capability identity.
    pub id: String,
    /// Extension-local name.
    pub name: String,
    /// Human-readable purpose.
    pub description: String,
    /// Requested built-in or namespaced tools.
    pub tools: Vec<String>,
    /// Requested extension skills.
    pub skills: Vec<String>,
    /// Requested extension MCP servers.
    pub mcp_servers: Vec<String>,
    /// Bounded Markdown body.
    pub prompt: String,
    /// Canonical source file.
    pub source: PathBuf,
}

/// One denied agent capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentDenial {
    /// Capability kind.
    pub kind: String,
    /// Requested name.
    pub name: String,
    /// Stable denial reason.
    pub reason: String,
}

/// Redaction-safe agent registry projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentDefinition {
    /// Canonical extension agent identity.
    pub id: String,
    /// Human-readable purpose.
    pub description: String,
    /// Canonical source file.
    pub source: String,
    /// Requested tool names.
    pub requested_tools: Vec<String>,
    /// Effective tool names after policy intersection.
    pub effective_tools: Vec<String>,
    /// Requested skill names.
    pub requested_skills: Vec<String>,
    /// Effective canonical skill identities.
    pub effective_skills: Vec<String>,
    /// Requested MCP server names.
    pub requested_mcp_servers: Vec<String>,
    /// Effective canonical MCP server identities.
    pub effective_mcp_servers: Vec<String>,
    /// Capabilities denied by extension or runtime policy.
    pub denied: Vec<AgentDenial>,
    /// False until a unified core subagent executor exists.
    pub executable: bool,
    /// Bounded prompt contribution captured in the snapshot.
    #[serde(skip_serializing)]
    pub prompt: String,
}

/// Strict agent discovery error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentError {
    code: &'static str,
    message: String,
}

impl AgentError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    /// Returns the stable machine-readable failure code.
    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for AgentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for AgentError {}

/// Immutable agent registry for one extension generation.
#[derive(Debug, Clone, Default)]
pub struct AgentRegistry {
    agents: Vec<AgentDefinition>,
    diagnostics: Vec<ExtensionDiagnostic>,
}

impl AgentRegistry {
    /// Builds a registry and intersects requests with extension and global policy.
    pub fn build(
        manager: &ExtensionManager,
        globally_allowed_tools: &BTreeSet<String>,
        workspace_trusted: bool,
        approval_mode: ApprovalMode,
    ) -> Self {
        let mut registry = Self::default();
        for extension in manager
            .list()
            .iter()
            .filter(|extension| extension.is_active)
        {
            match load_agent_files(
                &extension.name,
                &extension.path,
                &extension.agent_directories,
            ) {
                Ok(agents) => {
                    registry.agents.extend(agents.into_iter().map(|agent| {
                        resolve_agent(
                            extension,
                            agent,
                            globally_allowed_tools,
                            workspace_trusted,
                            approval_mode,
                        )
                    }));
                }
                Err(error) => registry
                    .diagnostics
                    .push(ExtensionDiagnostic::new(error.code(), error.to_string())),
            }
        }
        registry
            .agents
            .sort_by(|left, right| left.id.cmp(&right.id));
        registry
    }

    /// Returns all discovered agents.
    pub fn list(&self) -> &[AgentDefinition] {
        &self.agents
    }

    /// Looks up one canonical agent identity.
    pub fn info(&self, id: &str) -> Option<&AgentDefinition> {
        self.agents.iter().find(|agent| agent.id == id)
    }

    /// Returns strict discovery diagnostics.
    pub fn diagnostics(&self) -> &[ExtensionDiagnostic] {
        &self.diagnostics
    }
}

/// Loads strict agent files for manifest validation and runtime snapshots.
pub fn load_agent_files(
    extension: &str,
    package_root: &Path,
    directories: &[PathBuf],
) -> Result<Vec<ParsedAgent>, AgentError> {
    let package_root = package_root.canonicalize().map_err(|error| {
        AgentError::new(
            "extension_agent_path_unreadable",
            format!("failed to resolve extension root: {error}"),
        )
    })?;
    let mut files = Vec::new();
    for directory in directories {
        let directory = directory.canonicalize().map_err(|error| {
            AgentError::new(
                "extension_agent_path_unreadable",
                format!(
                    "failed to resolve agent directory {}: {error}",
                    directory.display()
                ),
            )
        })?;
        if !directory.starts_with(&package_root) {
            return Err(AgentError::new(
                "extension_agent_path_escape",
                format!(
                    "agent directory escapes extension package: {}",
                    directory.display()
                ),
            ));
        }
        for entry in fs::read_dir(&directory).map_err(|error| {
            AgentError::new(
                "extension_agent_path_unreadable",
                format!(
                    "failed to read agent directory {}: {error}",
                    directory.display()
                ),
            )
        })? {
            let path = entry
                .map_err(|error| {
                    AgentError::new(
                        "extension_agent_path_unreadable",
                        format!("failed to read agent directory entry: {error}"),
                    )
                })?
                .path();
            if path.extension().and_then(|value| value.to_str()) == Some("md") {
                files.push(path);
            }
        }
    }
    files.sort();
    let mut names = BTreeSet::new();
    let mut agents = Vec::new();
    for file in files {
        let agent = parse_agent_file(extension, &package_root, &file)?;
        if !names.insert(agent.name.clone()) {
            return Err(AgentError::new(
                "extension_agent_duplicate",
                format!("duplicate extension agent: {}", agent.name),
            ));
        }
        agents.push(agent);
    }
    Ok(agents)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AgentFrontmatter {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    skills: Vec<String>,
    #[serde(default)]
    mcp_servers: Vec<String>,
}

fn parse_agent_file(
    extension: &str,
    package_root: &Path,
    path: &Path,
) -> Result<ParsedAgent, AgentError> {
    let canonical = path.canonicalize().map_err(|error| {
        AgentError::new(
            "extension_agent_path_unreadable",
            format!("failed to resolve agent file {}: {error}", path.display()),
        )
    })?;
    if !canonical.starts_with(package_root) {
        return Err(AgentError::new(
            "extension_agent_path_escape",
            format!("agent file escapes extension package: {}", path.display()),
        ));
    }
    let bytes = fs::read(&canonical).map_err(|error| {
        AgentError::new(
            "extension_agent_invalid",
            format!("failed to read agent file {}: {error}", canonical.display()),
        )
    })?;
    if bytes.len() > MAX_AGENT_FILE_BYTES {
        return Err(AgentError::new(
            "extension_agent_too_large",
            format!(
                "agent file exceeds the {} byte limit: {}",
                MAX_AGENT_FILE_BYTES,
                canonical.display()
            ),
        ));
    }
    let content = String::from_utf8(bytes).map_err(|_| {
        AgentError::new(
            "extension_agent_not_utf8",
            format!("agent file is not UTF-8: {}", canonical.display()),
        )
    })?;
    let (frontmatter, prompt) = split_frontmatter(&content).map_err(|error| {
        let boundary = match error {
            FrontmatterBoundaryError::MissingOpening => "opening",
            FrontmatterBoundaryError::MissingClosing => "closing",
        };
        AgentError::new(
            "extension_agent_invalid",
            format!(
                "agent file is missing the {boundary} YAML frontmatter boundary: {}",
                canonical.display()
            ),
        )
    })?;
    let frontmatter: AgentFrontmatter = serde_yaml::from_str(frontmatter).map_err(|error| {
        AgentError::new(
            "extension_agent_frontmatter_invalid",
            format!(
                "invalid agent frontmatter in {}: {error}",
                canonical.display()
            ),
        )
    })?;
    validate_local_id(&frontmatter.name)
        .map_err(|error| AgentError::new(error.code(), error.to_string()))?;
    validate_unique_list(&frontmatter.tools, "tools")?;
    validate_unique_list(&frontmatter.skills, "skills")?;
    validate_unique_list(&frontmatter.mcp_servers, "mcpServers")?;
    let id = CapabilityId::new(extension, CapabilityKind::Agent, &frontmatter.name)
        .map_err(|error| AgentError::new(error.code(), error.to_string()))?
        .canonical();
    Ok(ParsedAgent {
        id,
        name: frontmatter.name,
        description: frontmatter.description,
        tools: frontmatter.tools,
        skills: frontmatter.skills,
        mcp_servers: frontmatter.mcp_servers,
        prompt: prompt.trim().to_string(),
        source: canonical,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FrontmatterBoundaryError {
    MissingOpening,
    MissingClosing,
}

fn split_frontmatter(content: &str) -> Result<(&str, &str), FrontmatterBoundaryError> {
    let content = content
        .strip_prefix("---\n")
        .ok_or(FrontmatterBoundaryError::MissingOpening)?;
    let boundary = content
        .find("\n---\n")
        .ok_or(FrontmatterBoundaryError::MissingClosing)?;
    Ok((&content[..boundary], &content[boundary + 5..]))
}

fn validate_unique_list(values: &[String], field: &str) -> Result<(), AgentError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.is_empty() || !seen.insert(value) {
            return Err(AgentError::new(
                "extension_agent_capability_invalid",
                format!("agent {field} contains an empty or duplicate value"),
            ));
        }
    }
    Ok(())
}

fn resolve_agent(
    extension: &Extension,
    agent: ParsedAgent,
    globally_allowed_tools: &BTreeSet<String>,
    workspace_trusted: bool,
    approval_mode: ApprovalMode,
) -> AgentDefinition {
    let declared_skills = extension
        .capabilities
        .iter()
        .filter(|capability| capability.contains("/skill/"))
        .cloned()
        .collect::<BTreeSet<_>>();
    let declared_mcp = extension
        .mcp_servers
        .iter()
        .map(|server| server.id.clone())
        .collect::<BTreeSet<_>>();
    let mut effective_tools = Vec::new();
    let mut effective_skills = Vec::new();
    let mut effective_mcp_servers = Vec::new();
    let mut denied = Vec::new();

    for tool in &agent.tools {
        let mut reason = None;
        if !globally_allowed_tools.contains(tool) {
            reason = Some("global_policy_denied");
        } else if !workspace_trusted && mutating_tool(tool) {
            reason = Some("workspace_untrusted");
        } else if approval_mode_restricts_mutation(approval_mode) && mutating_tool(tool) {
            reason = Some("approval_mode_restricted");
        }
        if let Some(reason) = reason {
            denied.push(AgentDenial {
                kind: "tool".to_string(),
                name: tool.clone(),
                reason: reason.to_string(),
            });
        } else {
            effective_tools.push(tool.clone());
        }
    }
    for skill in &agent.skills {
        let canonical = canonical_reference(&extension.name, "skill", skill);
        if declared_skills.contains(&canonical) {
            effective_skills.push(canonical);
        } else {
            denied.push(AgentDenial {
                kind: "skill".to_string(),
                name: skill.clone(),
                reason: "extension_capability_undeclared".to_string(),
            });
        }
    }
    for server in &agent.mcp_servers {
        let canonical = canonical_reference(&extension.name, "mcp", server);
        if !workspace_trusted {
            denied.push(AgentDenial {
                kind: "mcp".to_string(),
                name: server.clone(),
                reason: "workspace_untrusted".to_string(),
            });
        } else if declared_mcp.contains(&canonical) {
            effective_mcp_servers.push(canonical);
        } else {
            denied.push(AgentDenial {
                kind: "mcp".to_string(),
                name: server.clone(),
                reason: "extension_capability_undeclared".to_string(),
            });
        }
    }
    AgentDefinition {
        id: agent.id,
        description: agent.description,
        source: agent.source.to_string_lossy().into_owned(),
        requested_tools: agent.tools,
        effective_tools,
        requested_skills: agent.skills,
        effective_skills,
        requested_mcp_servers: agent.mcp_servers,
        effective_mcp_servers,
        denied,
        executable: false,
        prompt: agent.prompt,
    }
}

fn canonical_reference(extension: &str, kind: &str, value: &str) -> String {
    if value.starts_with(&format!("{extension}/{kind}/")) {
        value.to_string()
    } else {
        format!("{extension}/{kind}/{value}")
    }
}

fn mutating_tool(tool: &str) -> bool {
    matches!(tool, "shell" | "write_file" | "edit")
}

fn approval_mode_restricts_mutation(mode: ApprovalMode) -> bool {
    mode == ApprovalMode::Recommend
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::EXTENSION_CONFIG_FILENAME;
    use serde_json::json;

    fn package(root: &Path, agent_files: &[(&str, &str)]) -> ExtensionManager {
        let user = root.join("extensions");
        let system = root.join("system");
        let package = user.join("example.ops");
        let agents = package.join("agents");
        let skill = package.join("skills/triage");
        fs::create_dir_all(&agents).unwrap();
        fs::create_dir_all(&skill).unwrap();
        fs::create_dir_all(&system).unwrap();
        fs::write(
            skill.join("SKILL.md"),
            "---\nname: triage\ndescription: triage\n---\n\nTriage.",
        )
        .unwrap();
        for (name, content) in agent_files {
            fs::write(agents.join(name), content).unwrap();
        }
        fs::write(
            package.join(EXTENSION_CONFIG_FILENAME),
            serde_json::to_vec_pretty(&json!({
                "schemaVersion": 1,
                "name": "example.ops",
                "version": "1.0.0",
                "compatibility": {"cosh": ">=0.12.0"},
                "skills": ["skills"],
                "mcpServers": {
                    "inventory": {
                        "transport": "stdio",
                        "command": "missing-optional-server"
                    }
                },
                "agents": ["agents"]
            }))
            .unwrap(),
        )
        .unwrap();
        let mut manager = ExtensionManager::new_isolated_with_state(
            root.join("workspace"),
            Some(user),
            Some(system),
            root.join("state"),
        );
        manager.refresh();
        manager
    }

    fn valid_agent(extra: &str) -> String {
        format!(
            "---\nname: reviewer\ndescription: Review incidents\ntools:\n  - read_file\n  - shell\n  - unknown_tool\nskills:\n  - triage\n  - missing\nmcpServers:\n  - inventory\n  - missing\n{extra}---\n\nBounded prompt."
        )
    }

    #[test]
    fn strict_registry_computes_requested_effective_and_denied() {
        let temporary = tempfile::tempdir().unwrap();
        let manager = package(temporary.path(), &[("reviewer.md", &valid_agent(""))]);
        let allowed = ["read_file", "shell"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let registry = AgentRegistry::build(&manager, &allowed, true, ApprovalMode::Auto);
        let agent = &registry.list()[0];
        assert_eq!(agent.id, "example.ops/agent/reviewer");
        assert!(!agent.executable);
        assert_eq!(agent.effective_tools, ["read_file", "shell"]);
        assert_eq!(agent.effective_skills, ["example.ops/skill/triage"]);
        assert_eq!(agent.effective_mcp_servers, ["example.ops/mcp/inventory"]);
        assert!(
            agent
                .denied
                .iter()
                .any(|denial| denial.name == "unknown_tool"
                    && denial.reason == "global_policy_denied")
        );
        assert!(agent
            .denied
            .iter()
            .any(|denial| denial.name == "missing"
                && denial.reason == "extension_capability_undeclared"));
        assert_eq!(agent.prompt, "Bounded prompt.");
    }

    #[test]
    fn workspace_and_approval_policy_only_shrink_capabilities() {
        let temporary = tempfile::tempdir().unwrap();
        let manager = package(temporary.path(), &[("reviewer.md", &valid_agent(""))]);
        let allowed = ["read_file", "shell"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let registry = AgentRegistry::build(&manager, &allowed, false, ApprovalMode::Recommend);
        let agent = &registry.list()[0];
        assert_eq!(agent.effective_tools, ["read_file"]);
        assert!(agent.effective_mcp_servers.is_empty());
        assert!(agent
            .denied
            .iter()
            .any(|denial| denial.name == "shell" && denial.reason == "workspace_untrusted"));
        assert!(agent
            .denied
            .iter()
            .any(|denial| denial.kind == "mcp" && denial.reason == "workspace_untrusted"));
    }

    #[test]
    fn current_recommend_mode_restricts_mutating_agent_tools() {
        let temporary = tempfile::tempdir().unwrap();
        let manager = package(temporary.path(), &[("reviewer.md", &valid_agent(""))]);
        let allowed = ["read_file", "shell"]
            .into_iter()
            .map(str::to_string)
            .collect();
        let registry = AgentRegistry::build(&manager, &allowed, true, ApprovalMode::Recommend);
        let agent = &registry.list()[0];
        assert_eq!(agent.effective_tools, ["read_file"]);
        assert!(agent.denied.iter().any(|denial| {
            denial.name == "shell" && denial.reason == "approval_mode_restricted"
        }));
    }

    #[test]
    fn unknown_model_override_and_duplicate_name_fail_closed() {
        let model_root = tempfile::tempdir().unwrap();
        let manager = package(
            model_root.path(),
            &[(
                "reviewer.md",
                "---\nname: reviewer\nmodel: forced-model\n---\n\nPrompt.",
            )],
        );
        assert!(manager.list().is_empty());
        assert!(manager
            .catalog_diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "extension_agent_frontmatter_invalid"));

        let duplicate_root = tempfile::tempdir().unwrap();
        let manager = package(
            duplicate_root.path(),
            &[
                ("one.md", "---\nname: reviewer\n---\n\nOne."),
                ("two.md", "---\nname: reviewer\n---\n\nTwo."),
            ],
        );
        assert!(manager.list().is_empty());
        assert!(manager
            .catalog_diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "extension_agent_duplicate"));
    }

    #[test]
    fn missing_frontmatter_boundaries_report_safe_specific_diagnostics() {
        for (content, expected, secret) in [
            (
                "name: reviewer\n---\n\nopening-secret",
                "missing the opening YAML frontmatter boundary",
                "opening-secret",
            ),
            (
                "---\nname: reviewer\nclosing-secret",
                "missing the closing YAML frontmatter boundary",
                "closing-secret",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let manager = package(root.path(), &[("reviewer.md", content)]);
            let diagnostic = manager
                .catalog_diagnostics()
                .iter()
                .find(|diagnostic| diagnostic.code == "extension_agent_invalid")
                .unwrap();

            assert!(diagnostic.message.contains(expected));
            assert!(!diagnostic.message.contains(secret));
        }
    }

    #[test]
    fn oversized_and_escaping_agent_files_fail_closed() {
        let large_root = tempfile::tempdir().unwrap();
        let large = format!(
            "---\nname: reviewer\n---\n\n{}",
            "x".repeat(MAX_AGENT_FILE_BYTES)
        );
        let manager = package(large_root.path(), &[("large.md", &large)]);
        assert!(manager.list().is_empty());
        assert!(manager
            .catalog_diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code == "extension_agent_too_large"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            let escape_root = tempfile::tempdir().unwrap();
            let user = escape_root.path().join("extensions/example.ops");
            let agents = user.join("agents");
            let system = escape_root.path().join("system");
            fs::create_dir_all(&agents).unwrap();
            fs::create_dir_all(&system).unwrap();
            let outside = escape_root.path().join("outside.md");
            fs::write(&outside, "---\nname: escaped\n---\n\nNo.").unwrap();
            symlink(&outside, agents.join("escaped.md")).unwrap();
            fs::write(
                user.join(EXTENSION_CONFIG_FILENAME),
                r#"{"schemaVersion":1,"name":"example.ops","version":"1.0.0","compatibility":{"cosh":">=0.12.0"},"agents":["agents"]}"#,
            )
            .unwrap();
            let mut manager = ExtensionManager::new_isolated_with_state(
                escape_root.path().join("workspace"),
                Some(escape_root.path().join("extensions")),
                Some(system),
                escape_root.path().join("state"),
            );
            manager.refresh();
            assert!(manager.list().is_empty());
            assert!(manager
                .catalog_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code == "extension_agent_path_escape"));
        }
    }
}
