//! Non-recursive directory listing.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::Path;

use async_trait::async_trait;
use rustix::fs::{Dir, FileType};
use serde_json::Value;

use super::workspace_fs::WorkspaceFs;
use super::{Tool, ToolContext, ToolKind, ToolResult};

const MAX_ENTRIES: usize = 200;

pub(super) struct ListDirectoryTool;

#[async_trait]
impl Tool for ListDirectoryTool {
    fn name(&self) -> &str {
        "list_directory"
    }

    fn description(&self) -> &str {
        "List files and subdirectories directly inside a directory."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Directory to list (default: cwd)"
                }
            }
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let path = params
            .get("path")
            .and_then(Value::as_str)
            .unwrap_or(".")
            .to_string();
        let cwd = ctx.cwd.clone();
        let workspace = ctx.workspace()?;

        tokio::task::spawn_blocking(move || list_workspace_directory(&workspace, &cwd, &path))
            .await
            .map_err(|error| format!("Directory listing task failed: {error}"))?
    }
}

fn list_workspace_directory(
    workspace: &WorkspaceFs,
    cwd: &Path,
    path: &str,
) -> Result<ToolResult, String> {
    let directory = match workspace.open_directory(cwd, path) {
        Ok(directory) => directory,
        Err(error) => return Ok(ToolResult::error(error)),
    };
    let relative_path = directory.relative_path.clone();
    let display_path = directory.display_path;
    let reader = Dir::read_from(&directory.file)
        .map_err(|error| format!("Failed to list {}: {error}", display_path.display()))?;
    let mut entries = Vec::new();
    for entry in reader {
        let entry =
            entry.map_err(|error| format!("Failed to list {}: {error}", display_path.display()))?;
        let name = OsString::from_vec(entry.file_name().to_bytes().to_vec());
        if name == "." || name == ".." {
            continue;
        }
        let suffix = if is_directory_entry(workspace, &relative_path, &name, entry.file_type()) {
            "/"
        } else {
            ""
        };
        entries.push(format!("{}{suffix}", name.to_string_lossy()));
    }
    entries.sort_unstable();

    let total = entries.len();
    entries.truncate(MAX_ENTRIES);
    let mut output = if entries.is_empty() {
        format!("Directory is empty: {}", display_path.display())
    } else {
        entries.join("\n")
    };
    if total > MAX_ENTRIES {
        output.push_str(&format!("\n\n... {} entries omitted", total - MAX_ENTRIES));
    }
    Ok(ToolResult::success(output))
}

fn is_directory_entry(
    workspace: &WorkspaceFs,
    directory: &Path,
    name: &OsString,
    file_type: FileType,
) -> bool {
    if file_type == FileType::Directory {
        return true;
    }
    if file_type != FileType::Unknown {
        return false;
    }
    workspace.is_relative_directory(&directory.join(name))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[tokio::test]
    async fn lists_directories_with_suffix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "").unwrap();
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            dir.path().to_path_buf(),
        );

        let result = ListDirectoryTool
            .invoke(serde_json::json!({}), &ctx)
            .await
            .unwrap();

        assert_eq!(result.output, "Cargo.toml\nsrc/");
    }

    #[tokio::test]
    async fn rejects_directory_symlink_that_escapes_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        let outside = parent.path().join("outside");
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        symlink(&outside, root.join("outside-link")).unwrap();
        let ctx = ToolContext::new(root.clone(), "test".to_string(), root);

        let result = ListDirectoryTool
            .invoke(serde_json::json!({"path": "outside-link"}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("escapes workspace root"));
        assert!(!result.output.contains("secret.txt"));
    }

    #[test]
    fn unknown_search_only_directory_is_classified_without_read_access() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let nested = directory.path().join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o111)).unwrap();
        let workspace = WorkspaceFs::new(directory.path()).unwrap();

        let is_directory = is_directory_entry(
            &workspace,
            Path::new("."),
            &OsString::from("nested"),
            FileType::Unknown,
        );
        assert!(is_directory);
        std::fs::set_permissions(&nested, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
