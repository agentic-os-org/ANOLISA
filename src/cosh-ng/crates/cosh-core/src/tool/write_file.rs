use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolKind, ToolResult};

pub struct WriteFileTool;

#[async_trait]
impl Tool for WriteFileTool {
    fn name(&self) -> &str {
        "write_file"
    }

    fn description(&self) -> &str {
        "Write content to a file, creating it if it doesn't exist or overwriting if it does."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to write (absolute or relative to cwd)"
                },
                "content": {
                    "type": "string",
                    "description": "The content to write to the file"
                }
            },
            "required": ["path", "content"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::FileEdit
    }

    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let path_str = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing 'path' parameter")?;
        let content = params
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or("missing 'content' parameter")?;

        let placeholders = placeholder_markers(content);
        if !placeholders.is_empty() {
            return Ok(ToolResult::error(format!(
                "Write refused: placeholder(s) detected: {}. The file was not modified; use an \
                 interactive input path for credentials.",
                placeholders.join(", "),
            )));
        }
        let contains_sensitive = crate::redaction::contains_sensitive_text(content);

        let workspace = match ctx.workspace() {
            Ok(workspace) => workspace,
            Err(error) => return Ok(ToolResult::error(error)),
        };
        let cwd = ctx.cwd.clone();
        let path = path_str.to_owned();
        let prepared =
            tokio::task::spawn_blocking(move || workspace.prepare_write(&cwd, &path, true))
                .await
                .map_err(|error| format!("Write-file prepare task failed: {error}"))?;
        let target = match prepared {
            Ok(target) => target,
            Err(error) => return Ok(ToolResult::error(error)),
        };
        let display_path = target.display_path.clone();

        if let Err(error) =
            super::atomic_file::replace(&target, content.as_bytes().to_vec(), None).await
        {
            return Ok(ToolResult::error(format!(
                "Failed to write {}: {error}",
                display_path.display()
            )));
        }

        let lines = content.lines().count();
        let bytes = content.len();
        let warning = if contains_sensitive {
            "\nWarning: content appears to contain sensitive material; verify the destination and access permissions."
        } else {
            ""
        };
        Ok(ToolResult::success(format!(
            "Wrote {bytes} bytes ({lines} lines) to {}{warning}",
            display_path.display(),
        )))
    }
}

fn placeholder_markers(content: &str) -> Vec<&'static str> {
    let upper = content.to_ascii_uppercase();
    let mut markers = Vec::new();

    if upper.contains("<REDACTED") {
        markers.push("<redacted>");
    }
    if upper.contains("<SECRET>") {
        markers.push("<secret>");
    }
    if upper.contains("[REDACTED:") {
        markers.push("[REDACTED:...]");
    }
    if upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| {
            word.starts_with("YOUR_")
                && (word.ends_with("_KEY") || word.ends_with("_TOKEN") || word.ends_with("_SECRET"))
        })
    {
        markers.push("YOUR_*_KEY/TOKEN/SECRET");
    }
    if upper
        .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .any(|word| {
            word.starts_with("XXX_")
                && word.ends_with("_XXX")
                && ["_KEY_", "_TOKEN_", "_SECRET_"]
                    .iter()
                    .any(|marker| word.contains(marker))
        })
    {
        markers.push("XXX_*_(KEY|TOKEN|SECRET)_XXX");
    }

    markers
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    #[cfg(unix)]
    use std::os::unix::fs::symlink;
    #[cfg(unix)]
    use std::path::PathBuf;

    use super::*;

    fn test_ctx_in(dir: &Path) -> ToolContext {
        ToolContext::new(dir.to_path_buf(), "test".to_string(), dir.to_path_buf())
    }

    #[cfg(unix)]
    fn create_symlink_chain(dir: &Path, length: usize) -> (PathBuf, PathBuf) {
        let target = dir.join("target.txt");
        for index in (0..length).rev() {
            let link = dir.join(format!("link-{index}.txt"));
            let referent = if index + 1 == length {
                PathBuf::from("target.txt")
            } else {
                PathBuf::from(format!("link-{}.txt", index + 1))
            };
            symlink(referent, link).unwrap();
        }
        (dir.join("link-0.txt"), target)
    }

    #[tokio::test]
    async fn write_new_file() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool;
        let path = dir.path().join("test.txt");

        let result = tool
            .invoke(
                serde_json::json!({"path": path.to_str().unwrap(), "content": "hello world"}),
                &test_ctx_in(dir.path()),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("11 bytes"));

        let content = std::fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello world");
    }

    #[tokio::test]
    async fn write_creates_parent_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool;
        let path = dir.path().join("sub/dir/test.txt");

        let result = tool
            .invoke(
                serde_json::json!({"path": path.to_str().unwrap(), "content": "nested"}),
                &test_ctx_in(dir.path()),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(path.exists());
    }

    #[tokio::test]
    async fn write_relative_path() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool;

        let result = tool
            .invoke(
                serde_json::json!({"path": "relative.txt", "content": "rel"}),
                &test_ctx_in(dir.path()),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(dir.path().join("relative.txt").exists());
    }

    #[tokio::test]
    async fn write_rejects_parent_traversal_as_tool_error() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = parent.path().join("outside.txt");

        let result = WriteFileTool
            .invoke(
                serde_json::json!({"path": "../outside.txt", "content": "blocked"}),
                &test_ctx_in(&workspace),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("escapes workspace root"));
        assert!(!outside.exists());
    }

    #[tokio::test]
    async fn write_rejects_parent_traversal_when_creating_parents() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = parent.path().join("created-outside");

        let result = WriteFileTool
            .invoke(
                serde_json::json!({
                    "path": "../created-outside/nested.txt",
                    "content": "blocked"
                }),
                &test_ctx_in(&workspace),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(!outside.exists());
    }

    #[tokio::test]
    async fn write_rejects_absolute_external_path_as_tool_error() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        let original = std::fs::read(outside.path()).unwrap();

        let result = WriteFileTool
            .invoke(
                serde_json::json!({
                    "path": outside.path(),
                    "content": "blocked"
                }),
                &test_ctx_in(workspace.path()),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("escapes workspace root"));
        assert_eq!(std::fs::read(outside.path()).unwrap(), original);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_rejects_external_final_and_parent_symlinks() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        let outside = parent.path().join("outside");
        std::fs::create_dir(&workspace).unwrap();
        std::fs::create_dir(&outside).unwrap();
        let outside_file = outside.join("target.txt");
        std::fs::write(&outside_file, "stable").unwrap();

        symlink(&outside_file, workspace.join("file-link")).unwrap();
        let result = WriteFileTool
            .invoke(
                serde_json::json!({"path": "file-link", "content": "blocked"}),
                &test_ctx_in(&workspace),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert_eq!(std::fs::read_to_string(&outside_file).unwrap(), "stable");

        symlink(&outside, workspace.join("dir-link")).unwrap();
        let result = WriteFileTool
            .invoke(
                serde_json::json!({
                    "path": "dir-link/new/nested.txt",
                    "content": "blocked"
                }),
                &test_ctx_in(&workspace),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(!outside.join("new").exists());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_preserves_internal_file_and_directory_symlinks() {
        let workspace = tempfile::tempdir().unwrap();
        let target = workspace.path().join("target.txt");
        let directory = workspace.path().join("directory");
        std::fs::write(&target, "old").unwrap();
        std::fs::create_dir(&directory).unwrap();
        symlink("target.txt", workspace.path().join("file-link")).unwrap();
        symlink("directory", workspace.path().join("directory-link")).unwrap();

        let tool = WriteFileTool;
        let file_result = tool
            .invoke(
                serde_json::json!({"path": "file-link", "content": "new"}),
                &test_ctx_in(workspace.path()),
            )
            .await
            .unwrap();
        let directory_result = tool
            .invoke(
                serde_json::json!({
                    "path": "directory-link/nested.txt",
                    "content": "nested"
                }),
                &test_ctx_in(workspace.path()),
            )
            .await
            .unwrap();

        assert!(!file_result.is_error);
        assert!(!directory_result.is_error);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "new");
        assert_eq!(
            std::fs::read_to_string(directory.join("nested.txt")).unwrap(),
            "nested"
        );
        assert!(workspace.path().join("file-link").is_symlink());
        assert!(workspace.path().join("directory-link").is_symlink());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_rejects_directory_constrained_file_targets() {
        let workspace = tempfile::tempdir().unwrap();
        let target = workspace.path().join("target.txt");
        let trailing_link = workspace.path().join("trailing-link");
        let dot_link = workspace.path().join("dot-link");
        std::fs::write(&target, "stable").unwrap();
        symlink("target.txt/", &trailing_link).unwrap();
        symlink("target.txt/.", &dot_link).unwrap();

        for path in ["target.txt/", "target.txt/.", "trailing-link", "dot-link"] {
            let result = WriteFileTool
                .invoke(
                    serde_json::json!({"path": path, "content": "blocked"}),
                    &test_ctx_in(workspace.path()),
                )
                .await
                .unwrap();

            assert!(result.is_error, "write unexpectedly succeeded for {path}");
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "stable");
        }

        let result = WriteFileTool
            .invoke(
                serde_json::json!({"path": "missing.txt/", "content": "blocked"}),
                &test_ctx_in(workspace.path()),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(!workspace.path().join("missing.txt").exists());
        assert!(trailing_link.is_symlink());
        assert!(dot_link.is_symlink());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_rejects_dangling_external_symlink() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        let outside = parent.path().join("not-created.txt");
        std::fs::create_dir(&workspace).unwrap();
        symlink(&outside, workspace.join("dangling-link")).unwrap();

        let result = WriteFileTool
            .invoke(
                serde_json::json!({"path": "dangling-link", "content": "blocked"}),
                &test_ctx_in(&workspace),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(!outside.exists());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_uses_pinned_root_after_path_replacement() {
        let parent = tempfile::tempdir().unwrap();
        let workspace = parent.path().join("workspace");
        let moved = parent.path().join("moved-workspace");
        std::fs::create_dir(&workspace).unwrap();
        let context = test_ctx_in(&workspace);
        std::fs::rename(&workspace, &moved).unwrap();
        std::fs::create_dir(&workspace).unwrap();

        let result = WriteFileTool
            .invoke(
                serde_json::json!({"path": "result.txt", "content": "pinned"}),
                &context,
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(
            std::fs::read_to_string(moved.join("result.txt")).unwrap(),
            "pinned"
        );
        assert!(!workspace.join("result.txt").exists());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn prepared_target_rejects_final_symlink_swap() {
        let parent = tempfile::tempdir().unwrap();
        let workspace_path = parent.path().join("workspace");
        let outside = parent.path().join("outside.txt");
        std::fs::create_dir(&workspace_path).unwrap();
        std::fs::write(workspace_path.join("inside.txt"), "old").unwrap();
        std::fs::write(&outside, "outside").unwrap();
        symlink("inside.txt", workspace_path.join("link.txt")).unwrap();

        let workspace = super::super::workspace_fs::WorkspaceFs::new(&workspace_path).unwrap();
        let target = workspace
            .prepare_write(&workspace_path, "link.txt", false)
            .unwrap();
        assert_eq!(
            super::super::atomic_file::read_snapshot(&target)
                .await
                .unwrap()
                .bytes(),
            b"old"
        );
        let resolved_leaf = workspace_path.join("inside.txt");
        std::fs::remove_file(&resolved_leaf).unwrap();
        symlink(&outside, &resolved_leaf).unwrap();
        assert!(super::super::atomic_file::read_snapshot(&target)
            .await
            .is_err());

        let result = super::super::atomic_file::replace(&target, b"new".to_vec(), None).await;
        assert!(result.is_err());

        assert_eq!(
            std::fs::read_link(&resolved_leaf).unwrap(),
            outside.as_path()
        );
        assert_eq!(std::fs::read_to_string(outside).unwrap(), "outside");
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_follows_relative_dangling_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target_dir = dir.path().join("target");
        std::fs::create_dir(&target_dir).unwrap();
        let link = dir.path().join("relative-link.txt");
        let target = target_dir.join("created.txt");
        symlink("target/created.txt", &link).unwrap();

        let result = WriteFileTool
            .invoke(
                serde_json::json!({"path": link, "content": "relative target"}),
                &test_ctx_in(dir.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "relative target");
        assert!(link.is_symlink());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_follows_absolute_dangling_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("absolute-link.txt");
        let target = dir.path().join("created.txt");
        symlink(&target, &link).unwrap();

        let result = WriteFileTool
            .invoke(
                serde_json::json!({"path": link, "content": "absolute target"}),
                &test_ctx_in(dir.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "absolute target");
        assert!(link.is_symlink());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_follows_symlink_chain_at_system_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (link, target) = create_symlink_chain(dir.path(), 40);

        let result = WriteFileTool
            .invoke(
                serde_json::json!({"path": link, "content": "forty links"}),
                &test_ctx_in(dir.path()),
            )
            .await
            .unwrap();

        assert!(!result.is_error);
        assert_eq!(std::fs::read_to_string(target).unwrap(), "forty links");
        assert!(link.is_symlink());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn write_rejects_symlink_chain_over_system_limit() {
        let dir = tempfile::tempdir().unwrap();
        let (link, target) = create_symlink_chain(dir.path(), 41);

        let result = WriteFileTool
            .invoke(
                serde_json::json!({"path": link, "content": "forty-one links"}),
                &test_ctx_in(dir.path()),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result
            .output
            .to_ascii_lowercase()
            .contains("too many symbolic links"));
        assert!(!target.exists());
        assert!(link.is_symlink());
    }

    #[tokio::test]
    async fn write_redacted_content_is_refused_before_fs_side_effects() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool;
        let parent = dir.path().join("new");
        let path = parent.join("settings.json");
        let content = r#"{\"token\": \"<redacted>\"}"#;

        let result = tool
            .invoke(
                serde_json::json!({"path": path, "content": content}),
                &test_ctx_in(dir.path()),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.starts_with("Write refused:"));
        assert!(result.output.contains("<redacted>"));
        assert!(result.output.contains("interactive input path"));
        assert!(result.output.contains("file was not modified"));
        assert!(!path.exists());
        assert!(!parent.exists());
    }

    #[tokio::test]
    async fn write_new_placeholders_is_refused_before_fs_side_effects() {
        for (name, content) in [
            ("secret.txt", "value=<SeCrEt>"),
            ("token.txt", "value=XXX_SERVICE_TOKEN_XXX"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let parent = dir.path().join("new");
            let path = parent.join(name);

            let result = WriteFileTool
                .invoke(
                    serde_json::json!({"path": path, "content": content}),
                    &test_ctx_in(dir.path()),
                )
                .await
                .unwrap();

            assert!(result.is_error, "{content} should be refused");
            assert!(result.output.starts_with("Write refused:"));
            assert!(!path.exists());
            assert!(!parent.exists());
        }
    }

    #[tokio::test]
    async fn write_sensitive_content_warns_without_echoing_it() {
        for (name, content) in [
            ("aws.env", "AWS_ACCESS_KEY_ID=AKIA1234567890ABCDEF"),
            ("sts.env", "AWS_ACCESS_KEY_ID=ASIA1234567890ABCDEF"),
            (
                "key.pem",
                "-----BEGIN PRIVATE KEY-----\nsecret\n-----END PRIVATE KEY-----",
            ),
            (
                "orphan-key-end.pem",
                "MII...\n-----END RSA PRIVATE KEY-----",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(name);

            let result = WriteFileTool
                .invoke(
                    serde_json::json!({"path": path, "content": content}),
                    &test_ctx_in(dir.path()),
                )
                .await
                .unwrap();

            assert!(!result.is_error, "{content}");
            assert!(result.output.contains("sensitive material"));
            assert!(!result.output.contains(content));
            assert_eq!(std::fs::read_to_string(path).unwrap(), content);
        }
    }

    #[tokio::test]
    async fn refused_write_does_not_overwrite_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let tool = WriteFileTool;
        let path = dir.path().join("settings.json");
        std::fs::write(&path, "valid configuration").unwrap();

        let result = tool
            .invoke(
                serde_json::json!({"path": path, "content": "token=YOUR_API_TOKEN"}),
                &test_ctx_in(dir.path()),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("YOUR_*_KEY/TOKEN/SECRET"));
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "valid configuration"
        );
    }

    #[test]
    fn detects_supported_placeholder_markers() {
        let markers = placeholder_markers(
            "<REDACTED private key block> [redacted: token] <secret> YOUR_API_KEY \
             YOUR_ACCESS_TOKEN YOUR_DB_SECRET XXX_API_KEY_XXX XXX_SERVICE_TOKEN_XXX \
             XXX_CLIENT_SECRET_XXX",
        );

        assert_eq!(
            markers,
            vec![
                "<redacted>",
                "<secret>",
                "[REDACTED:...]",
                "YOUR_*_KEY/TOKEN/SECRET",
                "XXX_*_(KEY|TOKEN|SECRET)_XXX"
            ]
        );
    }

    #[test]
    fn ignores_non_placeholder_content() {
        assert!(placeholder_markers("configured-token-value").is_empty());
    }
}
