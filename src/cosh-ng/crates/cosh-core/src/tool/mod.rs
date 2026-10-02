pub(crate) mod ask_user_question;
mod atomic_file;
pub mod edit;
mod file_patterns;
mod glob;
pub mod grep;
mod list_directory;
pub mod mcp;
pub mod read_file;
mod read_many_files;
mod runtime_context;
mod save_memory;
pub mod shell;
pub mod shell_evidence;
pub mod skill;
pub mod todo;
mod web_fetch;
mod workspace_checkpoint_create;
mod workspace_fs;
pub mod write_file;

use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde_json::Value;

use crate::provider::ToolDeclaration;
use crate::skill::{SkillConfig, SkillManager};

/// Expand `~`, `~/...`, or `~user/...` to the appropriate home directory.
///
/// - `~` or `~/foo` → current user's home directory
/// - `~user/foo` → specified user's home directory (via passwd lookup)
/// - If the user is not found, falls back to treating the path as-is.
pub(super) fn expand_tilde(path_str: &str) -> PathBuf {
    if path_str == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(path_str))
    } else if let Some(rest) = path_str.strip_prefix("~/") {
        dirs::home_dir()
            .map(|home| home.join(rest.trim_start_matches(std::path::MAIN_SEPARATOR)))
            .unwrap_or_else(|| PathBuf::from(path_str))
    } else if let Some(user_path) = path_str.strip_prefix('~') {
        // ~user or ~user/rest
        let (username, rest) = match user_path.find('/') {
            Some(idx) => (&user_path[..idx], Some(&user_path[idx + 1..])),
            None => (user_path, None),
        };
        let home = nix::unistd::User::from_name(username)
            .ok()
            .flatten()
            .map(|u| u.dir);
        match (home, rest) {
            (Some(h), Some(r)) => h.join(r.trim_start_matches(std::path::MAIN_SEPARATOR)),
            (Some(h), None) => h,
            (None, _) => PathBuf::from(path_str),
        }
    } else {
        PathBuf::from(path_str)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolKind {
    ReadOnly,
    /// A network request that can reach services outside the local process.
    Network,
    FileEdit,
    ShellExec,
    ShellEvidence,
    /// An external tool discovered from a configured MCP server.
    Mcp,
    /// A side effect that only the negotiated Gateway host may execute.
    HostedSideEffect,
    /// A tool contributed by an extension-owned external runtime.
    External,
    Other,
}

pub struct ToolContext {
    pub cwd: PathBuf,
    pub session_id: String,
    pub project_root: PathBuf,
    workspace: SessionWorkspace,
    runtime: ToolRuntimeContext,
}

/// Runtime-owned metadata exposed to tools without placing dynamic values in
/// the provider's cached system prompt.
#[derive(Debug, Clone, Default)]
pub(crate) struct ToolRuntimeContext {
    pub(crate) model: String,
    pub(crate) approval_mode: String,
    pub(crate) session_resumed: bool,
    pub(crate) compaction_revision: u64,
    pub(crate) compacted_through: Option<usize>,
    pub(crate) tools: Vec<String>,
    pub(crate) active_extensions: Vec<String>,
}

#[derive(Clone)]
pub(crate) struct SessionWorkspace {
    root: Arc<PathBuf>,
    pinned: Arc<OnceLock<Arc<workspace_fs::WorkspaceFs>>>,
    permanent_error: Option<Arc<str>>,
    retry_missing: bool,
}

impl SessionWorkspace {
    pub(crate) fn try_new(root: &Path) -> Result<Self, String> {
        let workspace = Self::new(root);
        if let Some(error) = &workspace.permanent_error {
            Err(error.to_string())
        } else {
            Ok(workspace)
        }
    }

    pub(crate) fn new(root: &Path) -> Self {
        let pinned = Arc::new(OnceLock::new());
        let (permanent_error, retry_missing) = match workspace_fs::WorkspaceFs::open_root(root) {
            Ok(workspace) => {
                let _ = pinned.set(Arc::new(workspace));
                (None, false)
            }
            Err(workspace_fs::WorkspaceRootError::Missing(_)) => (None, true),
            Err(workspace_fs::WorkspaceRootError::Permanent(error)) => (Some(error.into()), false),
        };
        Self {
            root: Arc::new(root.to_path_buf()),
            pinned,
            permanent_error,
            retry_missing,
        }
    }

    /// Returns the identity derived from the pinned root when available.
    pub(crate) fn root(&self) -> &Path {
        self.pinned
            .get()
            .map_or(self.root.as_path(), |workspace| workspace.root())
    }

    fn get(&self) -> Result<Arc<workspace_fs::WorkspaceFs>, String> {
        if let Some(workspace) = self.pinned.get() {
            return Ok(Arc::clone(workspace));
        }
        if let Some(error) = &self.permanent_error {
            return Err(error.to_string());
        }
        if !self.retry_missing {
            return Err("workspace root is unavailable".to_string());
        }

        let workspace = Arc::new(
            workspace_fs::WorkspaceFs::open_root(&self.root).map_err(|error| error.to_string())?,
        );
        let _ = self.pinned.set(Arc::clone(&workspace));
        Ok(self.pinned.get().map_or(workspace, Arc::clone))
    }
}

impl ToolContext {
    pub(crate) fn new(cwd: PathBuf, session_id: String, project_root: PathBuf) -> Self {
        let workspace = SessionWorkspace::new(&project_root);
        Self {
            cwd,
            session_id,
            project_root,
            workspace,
            runtime: ToolRuntimeContext::default(),
        }
    }

    pub(crate) fn with_runtime(
        cwd: PathBuf,
        session_id: String,
        project_root: PathBuf,
        workspace: SessionWorkspace,
        runtime: ToolRuntimeContext,
    ) -> Self {
        Self {
            cwd,
            session_id,
            project_root,
            workspace,
            runtime,
        }
    }

    fn workspace(&self) -> Result<Arc<workspace_fs::WorkspaceFs>, String> {
        self.workspace.get()
    }
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub output: String,
    pub is_error: bool,
}

impl ToolResult {
    pub fn success(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: false,
        }
    }

    pub fn error(output: impl Into<String>) -> Self {
        Self {
            output: output.into(),
            is_error: true,
        }
    }
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn parameters_schema(&self) -> Value;
    fn kind(&self) -> ToolKind;
    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String>;
}

pub struct ToolRegistry {
    tools: HashMap<String, Box<dyn Tool>>,
    skill_manager: Option<Arc<SkillManager>>,
    ask_user_question_enabled: bool,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self {
            tools: HashMap::new(),
            skill_manager: None,
            ask_user_question_enabled: true,
        }
    }

    pub fn register(&mut self, tool: Box<dyn Tool>) {
        self.tools.insert(tool.name().to_string(), tool);
    }

    pub fn get(&self, name: &str) -> Option<&dyn Tool> {
        self.tools.get(name).map(|t| t.as_ref())
    }

    /// Returns whether a tool name has already been registered.
    pub fn contains(&self, name: &str) -> bool {
        self.tools.contains_key(name)
    }

    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.tools.keys().cloned().collect();
        if self.ask_user_question_enabled {
            names.push("ask_user_question".to_string());
        }
        names.sort();
        names
    }

    pub fn retain_selected_tools(&mut self, selection: &str) -> Result<(), String> {
        if selection == "default" {
            return Ok(());
        }

        let selected: std::collections::HashSet<_> = selection
            .split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .collect();
        let available: std::collections::HashSet<_> = self
            .tools
            .keys()
            .map(String::as_str)
            .chain(std::iter::once("ask_user_question"))
            .collect();
        let mut unknown: Vec<_> = selected.difference(&available).copied().collect();
        unknown.sort_unstable();
        if !unknown.is_empty() {
            return Err(format!("unknown tools: {}", unknown.join(",")));
        }

        self.tools
            .retain(|name, _| selected.contains(name.as_str()));
        self.ask_user_question_enabled = selected.contains("ask_user_question");
        Ok(())
    }

    pub fn supports_ask_user_question(&self) -> bool {
        self.ask_user_question_enabled
    }

    pub fn with_defaults(skill_manager: Arc<SkillManager>) -> Self {
        let mut registry = Self::new();
        registry.register(Box::new(shell::ShellTool));
        registry.register(Box::new(read_file::ReadFileTool::new()));
        registry.register(Box::new(write_file::WriteFileTool));
        registry.register(Box::new(edit::EditTool));
        registry.register(Box::new(grep::GrepTool));
        registry.register(Box::new(glob::GlobTool));
        registry.register(Box::new(list_directory::ListDirectoryTool));
        registry.register(Box::new(read_many_files::ReadManyFilesTool));
        registry.register(Box::new(runtime_context::RuntimeContextTool));
        registry.register(Box::new(save_memory::SaveMemoryTool));
        registry.register(Box::new(todo::TodoTool::new()));
        registry.register(Box::new(web_fetch::WebFetchTool::new()));
        registry.register(Box::new(skill::SkillTool::new(Arc::clone(&skill_manager))));
        registry.skill_manager = Some(skill_manager);
        registry
    }

    /// Returns the task-only audited tool inventory for Gateway brokered v1.
    ///
    /// The inventory is constructed directly instead of filtering the legacy
    /// registry so new legacy tools cannot become exposed by accident.
    pub(crate) fn gateway_brokered_v1() -> Self {
        let mut registry = Self::new();
        registry.ask_user_question_enabled = true;
        registry
    }

    /// Returns the closed checkpoint inventory for Gateway brokered v4.
    ///
    /// The hosted declaration has no local effect path. It can only suspend a
    /// provider call until the private Gateway wire returns a typed result.
    pub(crate) fn gateway_brokered_checkpoint_v1() -> Self {
        let mut registry = Self::gateway_brokered_v1();
        registry.register(Box::new(
            workspace_checkpoint_create::WorkspaceCheckpointCreateTool,
        ));
        registry
    }

    /// Returns the closed approval-gated workspace-write inventory for v5.
    pub(crate) fn gateway_brokered_workspace_write_v1() -> Self {
        let mut registry = Self::gateway_brokered_v1();
        registry.register(Box::new(write_file::WriteFileTool));
        registry
    }

    pub fn with_shell_evidence(mut self) -> Self {
        self.register(Box::new(
            read_file::ReadFileTool::with_shell_evidence_tool_guidance(),
        ));
        self.register(Box::new(shell_evidence::ShellEvidenceTool));
        self
    }

    /// Convenience constructor for tests that don't need a real SkillManager.
    #[cfg(test)]
    pub fn with_defaults_for_test() -> Self {
        let mgr = SkillManager::new(PathBuf::from("/tmp"), vec![], vec![]);
        Self::with_defaults(mgr)
    }

    /// Return `(name, description)` pairs for all currently loaded skills.
    /// Used to inject an `# Available Skills` section into the system prompt
    /// so the LLM can proactively discover and invoke skills.
    /// Disabled skills are excluded from the list (agent cannot see them).
    pub async fn skill_summaries(&self) -> Vec<(String, String)> {
        let Some(mgr) = &self.skill_manager else {
            return Vec::new();
        };
        let disabled = crate::state::load_disabled(crate::state::SKILLS_STATE);
        mgr.list()
            .await
            .into_iter()
            .filter(|s| !disabled.contains(&s.name))
            .map(|s| (s.name, s.description))
            .collect()
    }

    /// Look up a single skill by name from the underlying manager.
    /// Used by the hook system to populate `skill_context` (skill_name +
    /// file_path) on PreToolUse / PostToolUse for the `skill` tool, so
    /// extensions like agent-sec-core's skill-ledger can locate the skill
    /// on disk regardless of how the LLM phrased the name.
    pub async fn lookup_skill(&self, name: &str) -> Option<SkillConfig> {
        let mgr = self.skill_manager.as_ref()?;
        mgr.load(name).await
    }

    pub fn declarations(&self) -> Vec<ToolDeclaration> {
        let mut decls: Vec<_> = self
            .tools
            .values()
            .map(|t| ToolDeclaration {
                name: t.name().to_string(),
                description: t.description().to_string(),
                parameters: t.parameters_schema(),
            })
            .collect();
        if self.ask_user_question_enabled {
            decls.push(ask_user_question::declaration());
        }
        decls.sort_by(|a, b| a.name.cmp(&b.name));
        decls
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn current_user() -> Option<nix::unistd::User> {
        nix::unistd::User::from_uid(nix::unistd::Uid::current())
            .ok()
            .flatten()
    }

    struct DummyTool;

    #[async_trait]
    impl Tool for DummyTool {
        fn name(&self) -> &str {
            "dummy"
        }
        fn description(&self) -> &str {
            "A dummy tool for testing"
        }
        fn parameters_schema(&self) -> Value {
            serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string" }
                },
                "required": ["input"]
            })
        }
        fn kind(&self) -> ToolKind {
            ToolKind::ReadOnly
        }
        async fn invoke(&self, params: Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
            let input = params
                .get("input")
                .and_then(|v| v.as_str())
                .unwrap_or("none");
            Ok(ToolResult::success(format!("echo: {input}")))
        }
    }

    #[test]
    fn registry_register_and_get() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool));
        assert!(registry.get("dummy").is_some());
        assert!(registry.get("nonexistent").is_none());
    }

    #[test]
    fn registry_names() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool));
        let names = registry.names();
        assert_eq!(names, vec!["ask_user_question", "dummy"]);
    }

    #[test]
    fn registry_declarations() {
        let mut registry = ToolRegistry::new();
        registry.register(Box::new(DummyTool));
        let decls = registry.declarations();
        assert_eq!(decls.len(), 2);
        assert!(decls.iter().any(|d| d.name == "dummy"));
        assert!(decls.iter().any(|d| d.name == "ask_user_question"));
    }

    #[test]
    fn empty_tool_selection_removes_builtin_and_pseudo_tools() {
        let mut registry = ToolRegistry::with_defaults_for_test();

        registry
            .retain_selected_tools("")
            .expect("empty selection is valid");

        assert!(registry.names().is_empty());
        assert!(registry.declarations().is_empty());
    }

    #[test]
    fn named_tool_selection_is_consistent_across_names_and_declarations() {
        let mut registry = ToolRegistry::with_defaults_for_test();

        registry
            .retain_selected_tools("read_file,ask_user_question")
            .expect("known selection");

        assert_eq!(
            registry.names(),
            vec!["ask_user_question".to_string(), "read_file".to_string()]
        );
        let declaration_names: Vec<_> = registry
            .declarations()
            .into_iter()
            .map(|declaration| declaration.name)
            .collect();
        assert_eq!(declaration_names, registry.names());
    }

    #[test]
    fn default_tool_selection_preserves_existing_tools() {
        let mut registry = ToolRegistry::with_defaults_for_test();
        let before = registry.names();

        registry
            .retain_selected_tools("default")
            .expect("default selection");

        assert_eq!(registry.names(), before);
    }

    #[test]
    fn defaults_include_restored_core_tools() {
        let registry = ToolRegistry::with_defaults_for_test();

        for name in [
            "glob",
            "list_directory",
            "read_many_files",
            "runtime_context",
            "save_memory",
            "web_fetch",
        ] {
            assert!(registry.contains(name), "missing default tool: {name}");
        }
    }

    #[test]
    fn brokered_inventory_is_explicit_and_task_only() {
        let registry = ToolRegistry::gateway_brokered_v1();

        assert_eq!(registry.names(), vec!["ask_user_question"]);
        assert!(registry.supports_ask_user_question());
        assert!(!registry.contains("workspace_checkpoint_create"));
        for legacy_side_effect in ["shell", "write_file", "edit", "save_memory"] {
            assert!(!registry.contains(legacy_side_effect));
        }
    }

    #[test]
    fn checkpoint_brokered_inventory_is_explicit_and_hosted() {
        let registry = ToolRegistry::gateway_brokered_checkpoint_v1();

        assert_eq!(
            registry.names(),
            vec!["ask_user_question", "workspace_checkpoint_create"]
        );
        assert_eq!(
            registry.get("workspace_checkpoint_create").map(Tool::kind),
            Some(ToolKind::HostedSideEffect)
        );
        for forbidden_local_effect in ["shell", "write_file", "edit", "save_memory"] {
            assert!(!registry.contains(forbidden_local_effect));
        }
    }

    #[test]
    fn workspace_write_inventory_is_explicit_and_local() {
        let registry = ToolRegistry::gateway_brokered_workspace_write_v1();

        assert_eq!(registry.names(), vec!["ask_user_question", "write_file"]);
        assert_eq!(
            registry.get("write_file").map(Tool::kind),
            Some(ToolKind::FileEdit)
        );
        for forbidden_tool in [
            "shell",
            "edit",
            "read_file",
            "save_memory",
            "workspace_checkpoint_create",
        ] {
            assert!(!registry.contains(forbidden_tool));
        }
    }

    #[test]
    fn unknown_tool_selection_is_rejected() {
        let mut registry = ToolRegistry::with_defaults_for_test();

        let error = registry
            .retain_selected_tools("read_file,missing_tool")
            .expect_err("unknown tool must fail closed");

        assert!(error.contains("missing_tool"));
    }

    #[test]
    fn cosh_shell_evidence_is_opt_in() {
        let registry = ToolRegistry::new();
        assert!(registry.get("cosh_shell_evidence").is_none());

        let registry = ToolRegistry::new().with_shell_evidence();
        let tool = registry
            .get("cosh_shell_evidence")
            .expect("shell evidence tool");
        assert_eq!(tool.kind(), ToolKind::ShellEvidence);

        let decls = registry.declarations();
        let decl = decls
            .iter()
            .find(|d| d.name == "cosh_shell_evidence")
            .expect("declaration");
        assert_eq!(decl.parameters["required"][0], "action");
        assert_eq!(
            decl.parameters["properties"]["action"]["enum"][0],
            "list_commands"
        );
        assert_eq!(
            decl.parameters["properties"]["action"]["enum"][1],
            "read_output"
        );
    }

    #[tokio::test]
    async fn tool_invoke() {
        let tool = DummyTool;
        let ctx = ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        );
        let result = tool
            .invoke(serde_json::json!({"input": "hello"}), &ctx)
            .await
            .unwrap();
        assert_eq!(result.output, "echo: hello");
        assert!(!result.is_error);
    }

    #[test]
    fn missing_workspace_is_pinned_after_creation() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        let workspace = SessionWorkspace::new(&root);
        assert!(workspace.get().is_err());

        std::fs::create_dir(&root).unwrap();
        let pinned = workspace.get().unwrap();

        assert!(pinned.open_directory(&root, ".").is_ok());
    }

    #[test]
    fn tool_result_constructors() {
        let ok = ToolResult::success("done");
        assert!(!ok.is_error);
        assert_eq!(ok.output, "done");

        let err = ToolResult::error("failed");
        assert!(err.is_error);
        assert_eq!(err.output, "failed");
    }

    #[test]
    fn expand_tilde_bare_tilde() {
        let result = expand_tilde("~");
        let home = dirs::home_dir().expect("home dir should exist");
        assert_eq!(result, home);
    }

    #[test]
    fn expand_tilde_with_subpath() {
        let result = expand_tilde("~/foo/bar");
        let home = dirs::home_dir().expect("home dir should exist");
        assert_eq!(result, home.join("foo/bar"));
        assert_eq!(expand_tilde("~/src/*.rs"), home.join("src/*.rs"));
    }

    #[test]
    fn expand_tilde_with_repeated_separator() {
        let result = expand_tilde("~//tmp/file");
        let home = dirs::home_dir().expect("home dir should exist");
        assert_eq!(result, home.join("tmp/file"));
    }

    #[test]
    fn expand_tilde_user_root() {
        if let Some(user) = current_user() {
            let tilde_user = format!("~{}", user.name);
            let result = expand_tilde(&tilde_user);
            assert_eq!(result, user.dir);
        }
    }

    #[test]
    fn expand_tilde_user_with_subpath() {
        if let Some(user) = current_user() {
            let tilde_user = format!("~{}/documents/file.txt", user.name);
            let result = expand_tilde(&tilde_user);
            assert_eq!(result, user.dir.join("documents/file.txt"));
        }
    }

    #[test]
    fn expand_tilde_user_with_repeated_separator() {
        if let Some(user) = current_user() {
            let tilde_user = format!("~{}//tmp/file", user.name);
            let result = expand_tilde(&tilde_user);
            assert_eq!(result, user.dir.join("tmp/file"));
        }
    }

    #[test]
    fn expand_tilde_unknown_user_falls_back() {
        let result = expand_tilde("~nonexistent_user_xyz_12345/file.txt");
        assert_eq!(
            result,
            PathBuf::from("~nonexistent_user_xyz_12345/file.txt")
        );
    }

    #[test]
    fn expand_tilde_no_tilde_passthrough() {
        let result = expand_tilde("relative/path");
        assert_eq!(result, PathBuf::from("relative/path"));
    }
}
