use std::path::{Path, PathBuf};

const CONTEXT_FILE: &str = ".copilot-shell/CONTEXT.md";

/// Persistence scope for context shared with future sessions.
#[derive(Clone, Copy)]
pub(crate) enum ContextScope {
    /// User-wide context loaded for every project.
    Global,
    /// Context associated with the active project.
    Project,
}

impl ContextScope {
    /// Parses the scope names accepted by the memory tool.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "global" => Some(Self::Global),
            "project" => Some(Self::Project),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Global => "Global",
            Self::Project => "Project",
        }
    }
}

/// Resolves the context file for a scope.
///
/// Returns `None` when the global scope is requested but no home directory is
/// available.
pub(crate) fn context_path(scope: ContextScope, project_root: &Path) -> Option<PathBuf> {
    match scope {
        ContextScope::Global => dirs::home_dir().map(|home| home.join(CONTEXT_FILE)),
        ContextScope::Project => Some(project_root.join(CONTEXT_FILE)),
    }
}

pub struct ContextBuilder;

impl ContextBuilder {
    pub fn build_system_prompt(
        cwd: &Path,
        tool_names: &[String],
        skill_summaries: &[(String, String)],
        approval_mode: &str,
        output_language: Option<&str>,
    ) -> String {
        Self::build_system_prompt_with_extensions(
            cwd,
            tool_names,
            skill_summaries,
            approval_mode,
            output_language,
            None,
        )
    }

    pub fn build_system_prompt_with_extensions(
        cwd: &Path,
        tool_names: &[String],
        skill_summaries: &[(String, String)],
        approval_mode: &str,
        output_language: Option<&str>,
        extension_context: Option<&str>,
    ) -> String {
        let mut parts = Vec::new();

        parts.push(format!(
            "# Environment\n- OS: {}\n- Shell: {}\n- CWD: {}",
            std::env::consts::OS,
            std::env::var("SHELL").unwrap_or_else(|_| "unknown".to_string()),
            cwd.display(),
        ));

        if let Some(ctx) = Self::load_context(cwd) {
            parts.push(format!("# Context\n{ctx}"));
        }

        if let Some(context) = extension_context.filter(|context| !context.trim().is_empty()) {
            parts.push(format!("# Extension Contexts\n{context}"));
        }

        parts.push(format!("# Approval Mode\nCurrent mode: `{approval_mode}`"));

        if !tool_names.is_empty() {
            parts.push(format!("# Available Tools\n{}", tool_names.join(", ")));
        }

        if !skill_summaries.is_empty() {
            let list: Vec<String> = skill_summaries
                .iter()
                .map(|(name, desc)| format!("- **{}**: {}", name, desc))
                .collect();
            parts.push(format!(
                "# Available Skills\nThe following skills are available. \
                 To use a skill, call the `skill` tool with action `invoke` and the skill name. \
                 When skills are available, use a skill when it clearly matches the user's request. \
                 For troubleshooting or diagnostic requests about a running machine, service, command failure, \
                 performance, stability, resource usage, or operational incident, first inspect the available skill \
                 descriptions. If one clearly matches, make invoking that skill your first diagnostic action. \
                 Invoke the matching skill directly; do not list skills or run broad shell diagnostics first. \
                 Use broad ad-hoc shell investigation first only when no available skill clearly matches, \
                 or when the matching skill's instructions tell you to do so. \
                 If no available skill clearly applies, continue normally.\n{}",
                list.join("\n")
            ));
        }

        if let Some(lang) = output_language {
            parts.push(format!("# Output Language\nRespond in {lang}."));
        }

        parts.join("\n\n")
    }

    fn load_context(cwd: &Path) -> Option<String> {
        let paths = [ContextScope::Global, ContextScope::Project]
            .into_iter()
            .filter_map(|scope| context_path(scope, cwd).map(|path| (scope, path)));
        Self::load_context_paths(paths)
    }

    fn load_context_paths(
        paths: impl IntoIterator<Item = (ContextScope, PathBuf)>,
    ) -> Option<String> {
        let mut contexts = paths.into_iter().filter_map(|(scope, path)| {
            std::fs::read_to_string(path)
                .ok()
                .filter(|content| !content.trim().is_empty())
                .map(|content| format!("## {} Context\n{}", scope.label(), content.trim()))
        });
        let first = contexts.next()?;
        Some(contexts.fold(first, |mut combined, context| {
            combined.push_str("\n\n");
            combined.push_str(&context);
            combined
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn basic_system_prompt() {
        let cwd = PathBuf::from("/tmp/test-project");
        let tools = vec!["shell".to_string(), "read_file".to_string()];
        let prompt = ContextBuilder::build_system_prompt(&cwd, &tools, &[], "balanced", None);

        assert!(prompt.contains("/tmp/test-project"));
        assert!(prompt.contains("shell, read_file"));
        assert!(prompt.contains("balanced"));
    }

    #[test]
    fn prompt_with_language() {
        let cwd = PathBuf::from("/tmp");
        let prompt = ContextBuilder::build_system_prompt(&cwd, &[], &[], "trust", Some("Chinese"));

        assert!(prompt.contains("Respond in Chinese"));
    }

    #[test]
    fn labels_context_sources_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let global = dir.path().join("global.md");
        let project = dir.path().join("project.md");
        std::fs::write(&global, "shared preference").unwrap();
        std::fs::write(&project, "project convention").unwrap();

        let context = ContextBuilder::load_context_paths([
            (ContextScope::Global, global),
            (ContextScope::Project, project),
        ])
        .unwrap();

        assert_eq!(
            context,
            "## Global Context\nshared preference\n\n## Project Context\nproject convention"
        );
    }

    #[test]
    fn prompt_labels_project_context() {
        let dir = tempfile::tempdir().unwrap();
        let context_dir = dir.path().join(".copilot-shell");
        std::fs::create_dir(&context_dir).unwrap();
        std::fs::write(context_dir.join("CONTEXT.md"), "project marker").unwrap();

        let prompt = ContextBuilder::build_system_prompt(dir.path(), &[], &[], "auto", None);

        assert!(prompt.contains("# Context"));
        assert!(prompt.contains("## Project Context\nproject marker"));
    }

    #[test]
    fn prompt_with_skills() {
        let cwd = PathBuf::from("/tmp");
        let skills = vec![
            ("code-review".to_string(), "Review code changes".to_string()),
            ("deploy".to_string(), "Deploy to production".to_string()),
        ];
        let prompt = ContextBuilder::build_system_prompt(&cwd, &[], &skills, "auto", None);

        assert!(prompt.contains("# Available Skills"));
        assert!(prompt.contains("**code-review**: Review code changes"));
        assert!(prompt.contains("**deploy**: Deploy to production"));
        assert!(prompt.contains("call the `skill` tool"));
        assert!(prompt.contains("clearly matches the user's request"));
        assert!(prompt.contains("running machine, service, command failure"));
        assert!(prompt.contains("make invoking that skill your first diagnostic action"));
        assert!(prompt.contains("Invoke the matching skill directly"));
        assert!(prompt
            .contains("Use broad ad-hoc shell investigation first only when no available skill"));
        assert!(prompt.contains("continue normally"));
    }

    #[test]
    fn prompt_without_skills() {
        let cwd = PathBuf::from("/tmp");
        let prompt = ContextBuilder::build_system_prompt(&cwd, &[], &[], "auto", None);

        assert!(!prompt.contains("Available Skills"));
    }

    #[test]
    fn extension_context_follows_project_context_before_policy_sections() {
        let temporary = tempfile::tempdir().unwrap();
        let project_dir = temporary.path().join(".copilot-shell");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("CONTEXT.md"), "PROJECT-CONTEXT").unwrap();
        let prompt = ContextBuilder::build_system_prompt_with_extensions(
            temporary.path(),
            &[],
            &[],
            "balanced",
            None,
            Some("EXTENSION-CONTEXT"),
        );
        let project = prompt.find("PROJECT-CONTEXT").unwrap();
        let extension = prompt.find("EXTENSION-CONTEXT").unwrap();
        let approval = prompt.find("# Approval Mode").unwrap();
        assert!(project < extension);
        assert!(extension < approval);
    }
}
