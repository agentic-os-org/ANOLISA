use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncReadExt;

use super::{Tool, ToolContext, ToolKind, ToolResult};

const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;

pub struct ReadFileTool {
    terminal_output_guidance: &'static str,
}

impl ReadFileTool {
    pub fn new() -> Self {
        Self {
            terminal_output_guidance:
                "terminal-output:// refs are not files. Use fenced cosh-request output fallback in cosh-shell.",
        }
    }

    pub fn with_shell_evidence_tool_guidance() -> Self {
        Self {
            terminal_output_guidance:
                "terminal-output:// refs are not files. Use cosh_shell_evidence action=read_output with output_id.",
        }
    }
}

impl Default for ReadFileTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for ReadFileTool {
    fn name(&self) -> &str {
        "read_file"
    }

    fn description(&self) -> &str {
        "Read the contents of a file. Returns the file content with line numbers."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to read (absolute or relative to cwd)"
                },
                "offset": {
                    "type": "integer",
                    "description": "Line number to start reading from (0-based, default: 0)"
                },
                "limit": {
                    "type": "integer",
                    "description": "Maximum number of lines to read (default: 2000)"
                }
            },
            "required": ["path"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let path_str = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing 'path' parameter")?
            .to_string();

        if path_str.starts_with("terminal-output://") {
            return Ok(ToolResult::error(self.terminal_output_guidance));
        }

        let cwd = ctx.cwd.clone();
        let workspace = ctx.workspace()?;
        let opened = tokio::task::spawn_blocking(move || workspace.open_file(&cwd, &path_str))
            .await
            .map_err(|error| format!("Read-file open task failed: {error}"))?;
        let opened = match opened {
            Ok(opened) => opened,
            Err(error) => return Ok(ToolResult::error(error)),
        };
        let path = opened.display_path;
        let file = tokio::fs::File::from_std(opened.file);
        let metadata = file
            .metadata()
            .await
            .map_err(|e| format!("Failed to inspect {}: {e}", path.display()))?;
        if metadata.len() > MAX_FILE_BYTES as u64 {
            return Ok(ToolResult::error(format!(
                "File exceeds the {} MiB read limit: {}",
                MAX_FILE_BYTES / (1024 * 1024),
                path.display()
            )));
        }

        // The metadata check avoids reading known-large files, while the
        // bounded reader also protects against files that grow after stat.
        let mut reader = file.take((MAX_FILE_BYTES + 1) as u64);
        let mut bytes = Vec::new();
        reader
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
        if bytes.len() > MAX_FILE_BYTES {
            return Ok(ToolResult::error(format!(
                "File exceeds the {} MiB read limit: {}",
                MAX_FILE_BYTES / (1024 * 1024),
                path.display()
            )));
        }
        let content = String::from_utf8(bytes).map_err(|error| {
            format!(
                "Failed to read {}: file is not valid UTF-8: {}",
                path.display(),
                error.utf8_error()
            )
        })?;

        let offset = params.get("offset").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
        let limit = params.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;

        let requested_end = offset.saturating_add(limit);
        let mut output = String::new();
        let mut total = 0;
        for (index, line) in content.lines().enumerate() {
            total = index + 1;
            if index >= offset && index < requested_end {
                output.push_str(&format!("{}\t{line}\n", index + 1));
            }
        }

        let end = requested_end.min(total);
        if end < total {
            output.push_str(&format!(
                "\n... ({} more lines, {} total)\n",
                total - end,
                total
            ));
        }

        Ok(ToolResult::success(output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::path::Path;

    fn test_ctx(root: &Path) -> ToolContext {
        ToolContext::new(root.to_path_buf(), "test".to_string(), root.to_path_buf())
    }

    #[tokio::test]
    async fn read_existing_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.txt");
        std::fs::write(&path, "line1\nline2\nline3\n").unwrap();

        let tool = ReadFileTool::new();
        let result = tool
            .invoke(
                serde_json::json!({"path": path.to_str().unwrap()}),
                &test_ctx(directory.path()),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("1\tline1"));
        assert!(result.output.contains("2\tline2"));
        assert!(result.output.contains("3\tline3"));
    }

    #[tokio::test]
    async fn read_with_offset_and_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("input.txt");
        std::fs::write(&path, "a\nb\nc\nd\ne\n").unwrap();

        let tool = ReadFileTool::new();
        let result = tool
            .invoke(
                serde_json::json!({"path": path.to_str().unwrap(), "offset": 1, "limit": 2}),
                &test_ctx(directory.path()),
            )
            .await
            .unwrap();
        assert!(!result.is_error);
        assert!(result.output.contains("2\tb"));
        assert!(result.output.contains("3\tc"));
        assert!(!result.output.contains("1\ta"));
    }

    #[tokio::test]
    async fn rejects_file_larger_than_limit() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("large.txt");
        std::fs::File::create(&path)
            .unwrap()
            .set_len((MAX_FILE_BYTES + 1) as u64)
            .unwrap();

        let tool = ReadFileTool::new();
        let result = tool
            .invoke(
                serde_json::json!({"path": path.to_str().unwrap()}),
                &test_ctx(directory.path()),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("10 MiB read limit"));
    }

    #[tokio::test]
    async fn read_nonexistent_file() {
        let directory = tempfile::tempdir().unwrap();
        let tool = ReadFileTool::new();
        let result = tool
            .invoke(
                serde_json::json!({"path": "definitely_not_a_real_file_xyz"}),
                &test_ctx(directory.path()),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
    }

    #[tokio::test]
    async fn follows_internal_symlink_and_rejects_external_symlink() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(root.join("inside.txt"), "inside").unwrap();
        std::fs::write(parent.path().join("outside.txt"), "outside").unwrap();
        symlink("inside.txt", root.join("inside-link")).unwrap();
        symlink(parent.path().join("outside.txt"), root.join("outside-link")).unwrap();
        let tool = ReadFileTool::new();

        let result = tool
            .invoke(serde_json::json!({"path": "inside-link"}), &test_ctx(&root))
            .await
            .unwrap();
        assert!(result.output.contains("inside"));

        let result = tool
            .invoke(
                serde_json::json!({"path": "outside-link"}),
                &test_ctx(&root),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(
            result.output.contains("escapes workspace root"),
            "{}",
            result.output
        );
    }

    #[tokio::test]
    async fn root_replacement_follows_platform_confinement() {
        let parent = tempfile::tempdir().unwrap();
        let container = parent.path().join("container");
        let root = container.join("workspace");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("value.txt"), "trusted").unwrap();
        let ctx = test_ctx(&root);

        let moved = parent.path().join("moved");
        std::fs::rename(&container, &moved).unwrap();
        let replacement = parent.path().join("replacement");
        std::fs::create_dir_all(replacement.join("workspace")).unwrap();
        std::fs::write(replacement.join("workspace/value.txt"), "outside").unwrap();
        symlink(&replacement, &container).unwrap();

        let result = ReadFileTool::new()
            .invoke(serde_json::json!({"path": "value.txt"}), &ctx)
            .await
            .unwrap();

        #[cfg(target_os = "linux")]
        assert!(result.output.contains("trusted"));
        #[cfg(target_os = "macos")]
        {
            assert!(result.is_error);
            assert!(result.output.contains("Pinned workspace root was replaced"));
        }
        assert!(!result.output.contains("outside"));
    }

    #[tokio::test]
    async fn read_terminal_output_ref_fails_closed() {
        let tool = ReadFileTool::with_shell_evidence_tool_guidance();
        let result = tool
            .invoke(
                serde_json::json!({"path": "terminal-output://raw-session/cmd-1"}),
                &test_ctx(Path::new("/tmp")),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result
            .output
            .contains("terminal-output:// refs are not files"));
        assert!(result
            .output
            .contains("cosh_shell_evidence action=read_output"));
        assert!(!result.output.contains("fenced cosh-request"));
    }

    #[tokio::test]
    async fn read_terminal_output_ref_defaults_to_fenced_fallback_guidance() {
        let tool = ReadFileTool::new();
        let result = tool
            .invoke(
                serde_json::json!({"path": "terminal-output://raw-session/cmd-1"}),
                &test_ctx(Path::new("/tmp")),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("fenced cosh-request"));
        assert!(!result.output.contains("cosh_shell_evidence"));
    }
}
