//! Bounded multi-file reads with optional glob expansion.

use std::fs::File;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncReadExt;

use super::file_patterns::expand_file_paths;
use super::{Tool, ToolContext, ToolKind, ToolResult};

const MAX_FILES: usize = 50;
const MAX_FILE_BYTES: usize = 64 * 1024;
const MAX_TOTAL_BYTES: usize = 256 * 1024;

pub(super) struct ReadManyFilesTool;

#[async_trait]
impl Tool for ReadManyFilesTool {
    fn name(&self) -> &str {
        "read_many_files"
    }

    fn description(&self) -> &str {
        "Read and concatenate multiple text files. Paths may be exact files, directories, or glob patterns."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "paths": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "File paths, directories, or glob patterns to read"
                }
            },
            "required": ["paths"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let patterns = params
            .get("paths")
            .and_then(Value::as_array)
            .ok_or("missing 'paths' parameter")?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or_else(|| "'paths' entries must be strings".to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        if patterns.is_empty() {
            return Ok(ToolResult::error("'paths' must not be empty"));
        }

        let cwd = ctx.cwd.clone();
        let workspace = ctx.workspace()?;
        let discovery_workspace = workspace.clone();
        let matches = tokio::task::spawn_blocking(move || {
            expand_file_paths(&patterns, &cwd, &discovery_workspace, MAX_FILES)
        })
        .await
        .map_err(|error| format!("File discovery task failed: {error}"))?;
        let matches = match matches {
            Ok(matches) => matches,
            Err(error) => return Ok(ToolResult::error(error)),
        };
        let mut skipped = matches.skipped;
        if matches.paths.is_empty() {
            let mut output = no_matching_files_output(matches.truncated).to_string();
            append_skipped_files(&mut output, &skipped);
            return Ok(ToolResult::success(output));
        }

        let mut output = String::new();
        for path in matches.paths {
            let open_workspace = workspace.clone();
            let open_path = path.clone();
            let file = match tokio::task::spawn_blocking(move || {
                open_workspace.open_display_file(&open_path)
            })
            .await
            .map_err(|error| format!("File open task failed: {error}"))?
            {
                Ok(file) => file.file,
                Err(error) => {
                    skipped.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            let (bytes, file_truncated) = match read_bounded(file).await {
                Ok(result) => result,
                Err(error) => {
                    skipped.push(format!("{}: {error}", path.display()));
                    continue;
                }
            };
            let available = MAX_TOTAL_BYTES.saturating_sub(output.len());
            if available == 0 {
                break;
            }
            let limit = bytes.len().min(MAX_FILE_BYTES).min(available);
            let content = match text_prefix(&bytes[..limit]) {
                Some(content) => content,
                None => {
                    skipped.push(format!("{}: not a UTF-8 text file", path.display()));
                    continue;
                }
            };
            output.push_str(&format!("--- {} ---\n", path.display()));
            output.push_str(content);
            if !content.ends_with('\n') {
                output.push('\n');
            }
            if file_truncated || limit < bytes.len() {
                output.push_str("[file content truncated]\n");
            }
        }

        if matches.truncated || output.len() >= MAX_TOTAL_BYTES {
            output.push_str("\n[additional files omitted by output limits]\n");
        }
        append_skipped_files(&mut output, &skipped);
        Ok(ToolResult::success(output))
    }
}

fn append_skipped_files(output: &mut String, skipped: &[String]) {
    if skipped.is_empty() {
        return;
    }
    output.push_str("\nSkipped files:\n");
    output.push_str(&skipped.join("\n"));
    output.push('\n');
}

fn no_matching_files_output(truncated: bool) -> &'static str {
    if truncated {
        "No matching files found in the searched subset.\n\n\
         [additional files omitted by output limits]\n"
    } else {
        "No matching files found."
    }
}

async fn read_bounded(file: File) -> Result<(Vec<u8>, bool), std::io::Error> {
    let file = tokio::fs::File::from_std(file);
    let mut reader = file.take((MAX_FILE_BYTES + 1) as u64);
    let mut bytes = Vec::with_capacity(MAX_FILE_BYTES + 1);
    reader.read_to_end(&mut bytes).await?;
    let truncated = bytes.len() > MAX_FILE_BYTES;
    bytes.truncate(MAX_FILE_BYTES);
    Ok((bytes, truncated))
}

fn text_prefix(bytes: &[u8]) -> Option<&str> {
    if bytes.contains(&0) {
        return None;
    }
    match std::str::from_utf8(bytes) {
        Ok(content) => Some(content),
        Err(error) if error.error_len().is_none() => {
            std::str::from_utf8(&bytes[..error.valid_up_to()]).ok()
        }
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use super::*;

    #[tokio::test]
    async fn reads_multiple_files_with_boundaries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "alpha").unwrap();
        std::fs::write(dir.path().join("b.txt"), "beta").unwrap();
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            dir.path().to_path_buf(),
        );

        let result = ReadManyFilesTool
            .invoke(serde_json::json!({"paths": ["*.txt"]}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error);
        assert!(result.output.contains("--- "));
        assert!(result.output.contains("a.txt ---\nalpha"));
        assert!(result.output.contains("b.txt ---\nbeta"));
    }

    #[tokio::test]
    async fn reads_file_limit_after_path_only_discovery() {
        let dir = tempfile::tempdir().unwrap();
        for index in 0..MAX_FILES {
            std::fs::write(dir.path().join(format!("{index:02}.txt")), "text").unwrap();
        }
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            dir.path().to_path_buf(),
        );

        let result = ReadManyFilesTool
            .invoke(serde_json::json!({"paths": ["."]}), &ctx)
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert_eq!(result.output.matches("--- ").count(), MAX_FILES);
        assert!(!result.output.contains("additional files omitted"));
    }

    #[tokio::test]
    async fn bounds_large_files_during_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large.txt");
        std::fs::write(&path, vec![b'x'; MAX_FILE_BYTES * 4]).unwrap();

        let file = std::fs::File::open(&path).unwrap();
        let (bytes, truncated) = read_bounded(file).await.unwrap();

        assert_eq!(bytes.len(), MAX_FILE_BYTES);
        assert!(truncated);
    }

    #[tokio::test]
    async fn skips_binary_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("binary.dat"), [0, 159, 146, 150]).unwrap();
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            dir.path().to_path_buf(),
        );

        let result = ReadManyFilesTool
            .invoke(serde_json::json!({"paths": ["binary.dat"]}), &ctx)
            .await
            .unwrap();

        assert!(result.output.contains("not a UTF-8 text file"));
        assert!(!result.output.contains('\u{fffd}'));
    }

    #[tokio::test]
    async fn skips_unreadable_exact_file_and_reads_later_files() {
        use std::os::unix::fs::PermissionsExt;

        if nix::unistd::Uid::effective().as_raw() == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked.txt");
        std::fs::write(&locked, "locked").unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        std::fs::write(dir.path().join("readable.txt"), "readable").unwrap();
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            dir.path().to_path_buf(),
        );

        let result = ReadManyFilesTool
            .invoke(
                serde_json::json!({"paths": ["locked.txt", "readable.txt"]}),
                &ctx,
            )
            .await
            .unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("readable.txt ---\nreadable"));
        assert!(result.output.contains("Skipped files:"));
        assert!(result.output.contains("locked.txt: Permission denied"));
    }

    #[tokio::test]
    async fn skips_unreadable_exact_directory_and_reads_later_files() {
        use std::os::unix::fs::PermissionsExt;

        if nix::unistd::Uid::effective().as_raw() == 0 {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("locked-dir");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o111)).unwrap();
        std::fs::write(dir.path().join("readable.txt"), "readable").unwrap();
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            dir.path().to_path_buf(),
        );

        let result = ReadManyFilesTool
            .invoke(
                serde_json::json!({"paths": ["locked-dir", "readable.txt"]}),
                &ctx,
            )
            .await
            .unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        assert!(!result.is_error, "{}", result.output);
        assert!(result.output.contains("readable.txt ---\nreadable"));
        assert!(result.output.contains("Skipped files:"));
        assert!(result.output.contains("locked-dir: Permission denied"));
    }

    #[tokio::test]
    async fn rejects_exact_symlink_that_escapes_workspace() {
        let parent = tempfile::tempdir().unwrap();
        let root = parent.path().join("workspace");
        std::fs::create_dir(&root).unwrap();
        std::fs::write(parent.path().join("outside.txt"), "outside").unwrap();
        symlink(parent.path().join("outside.txt"), root.join("outside-link")).unwrap();
        let ctx = ToolContext::new(root.clone(), "test".to_string(), root);

        let result = ReadManyFilesTool
            .invoke(serde_json::json!({"paths": ["outside-link"]}), &ctx)
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("escapes workspace root"));
    }

    #[test]
    fn empty_truncated_discovery_is_not_definitive() {
        let output = no_matching_files_output(true);

        assert!(output.contains("searched subset"));
        assert!(output.contains("additional files omitted"));
    }
}
