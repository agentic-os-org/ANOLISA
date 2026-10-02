//! Glob-based file discovery.

use async_trait::async_trait;
use serde_json::Value;

use super::file_patterns::expand_file_paths;
use super::workspace_fs::WorkspaceFs;
use super::{Tool, ToolContext, ToolKind, ToolResult};

const MAX_RESULTS: usize = 100;

pub(super) struct GlobTool;

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find files by glob pattern, such as '**/*.rs'. Returns at most 100 sorted paths."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "pattern": {
                    "type": "string",
                    "description": "Glob pattern to match"
                },
                "path": {
                    "type": "string",
                    "description": "Directory to search from (default: cwd)"
                }
            },
            "required": ["pattern"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let pattern = params
            .get("pattern")
            .and_then(Value::as_str)
            .ok_or("missing 'pattern' parameter")?
            .to_string();
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_string();
        let cwd = ctx.cwd.clone();
        let workspace = ctx.workspace()?;

        tokio::task::spawn_blocking(move || glob_workspace(&pattern, &path, &cwd, &workspace))
            .await
            .map_err(|error| format!("Glob task failed: {error}"))?
    }
}

fn glob_workspace(
    pattern: &str,
    path: &str,
    cwd: &std::path::Path,
    workspace: &WorkspaceFs,
) -> Result<ToolResult, String> {
    let directory = match workspace.open_directory(cwd, path) {
        Ok(directory) => directory,
        Err(error) => return Ok(ToolResult::error(error)),
    };
    let base = directory.display_path.clone();
    let matches = expand_file_paths(&[pattern.to_string()], &base, workspace, MAX_RESULTS)?;
    if matches.paths.is_empty() {
        return Ok(ToolResult::success(empty_glob_output(
            pattern,
            &base,
            matches.truncated,
        )));
    }

    let mut output = matches
        .paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join("\n");
    if matches.truncated {
        output.push_str("\n\n... results truncated at 100 files");
    }
    Ok(ToolResult::success(output))
}

fn empty_glob_output(pattern: &str, base: &std::path::Path, truncated: bool) -> String {
    let mut output = format!(
        "No files found matching pattern \"{pattern}\" in {}",
        base.display()
    );
    if truncated {
        output.push_str("\n\n... results truncated before the full workspace was searched");
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    #[tokio::test]
    async fn finds_matching_files_only() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("one.rs"), "one").unwrap();
        std::fs::write(dir.path().join("nested/two.rs"), "two").unwrap();
        std::fs::write(dir.path().join("three.txt"), "three").unwrap();
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            PathBuf::from(dir.path()),
        );

        let result = GlobTool
            .invoke(serde_json::json!({"pattern": "**/*.rs"}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("one.rs"));
        assert!(result.output.contains("two.rs"));
        assert!(!result.output.contains("three.txt"));
    }

    #[tokio::test]
    async fn does_not_follow_directory_symlink_outside_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        let outside = parent.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.rs"), "secret").unwrap();
        symlink(&outside, root.join("outside-link")).unwrap();
        let ctx = ToolContext::new(root.clone(), "test".to_string(), root);

        let result = GlobTool
            .invoke(serde_json::json!({"pattern": "**/*.rs"}), &ctx)
            .await
            .unwrap();

        assert!(!result.output.contains("secret.rs"));
    }

    #[tokio::test]
    async fn supports_absolute_patterns() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("src");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("lib.rs"), "lib").unwrap();
        let pattern = source.join("*.rs").to_string_lossy().to_string();
        let ctx = ToolContext::new(
            directory.path().to_path_buf(),
            "test".to_string(),
            directory.path().to_path_buf(),
        );

        let result = GlobTool
            .invoke(serde_json::json!({"pattern": pattern}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("lib.rs"));
    }

    #[tokio::test]
    async fn lists_unreadable_matching_file_names() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("locked.rs");
        std::fs::write(&path, "private").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let ctx = ToolContext::new(
            directory.path().to_path_buf(),
            "test".to_string(),
            directory.path().to_path_buf(),
        );

        let result = GlobTool
            .invoke(serde_json::json!({"pattern": "*.rs"}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("locked.rs"));
        assert!(!result.output.contains("results truncated"));
    }

    #[test]
    fn empty_truncated_result_is_not_definitive() {
        let output = empty_glob_output("*.rs", std::path::Path::new("/workspace"), true);

        assert!(output.contains("results truncated"));
        assert!(output.contains("full workspace"));
    }
}
