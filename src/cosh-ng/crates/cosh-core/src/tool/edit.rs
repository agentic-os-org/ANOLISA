use async_trait::async_trait;
use serde_json::Value;

use super::atomic_file::{self, ReplaceError};
use super::{Tool, ToolContext, ToolKind, ToolResult};

pub struct EditTool;

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Edit a file by replacing exact text, including whitespace and indentation. By default, old_string must occur exactly once, counting overlapping matches."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "path": {
                    "type": "string",
                    "description": "Path to the file to edit"
                },
                "old_string": {
                    "type": "string",
                    "description": "The exact string to find and replace"
                },
                "new_string": {
                    "type": "string",
                    "description": "The string to replace it with"
                },
                "replace_all": {
                    "type": "boolean",
                    "description": "If true, replace all non-overlapping matches from left to right (default: false)"
                }
            },
            "required": ["path", "old_string", "new_string"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::FileEdit
    }

    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        self.invoke_with_snapshot_hook(params, ctx, || async {})
            .await
    }
}

impl EditTool {
    async fn invoke_with_snapshot_hook<AfterSnapshot, AfterSnapshotFuture>(
        &self,
        params: Value,
        ctx: &ToolContext,
        after_snapshot: AfterSnapshot,
    ) -> Result<ToolResult, String>
    where
        AfterSnapshot: FnOnce() -> AfterSnapshotFuture,
        AfterSnapshotFuture: std::future::Future<Output = ()>,
    {
        let path_str = params
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or("missing 'path' parameter")?;
        let old_string = params
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or("missing 'old_string' parameter")?;
        let new_string = params
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or("missing 'new_string' parameter")?;
        let replace_all = params
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let workspace = match ctx.workspace() {
            Ok(workspace) => workspace,
            Err(error) => return Ok(ToolResult::error(error)),
        };
        let cwd = ctx.cwd.clone();
        let path = path_str.to_owned();
        let prepared =
            tokio::task::spawn_blocking(move || workspace.prepare_write(&cwd, &path, false))
                .await
                .map_err(|error| format!("Edit prepare task failed: {error}"))?;
        let target = match prepared {
            Ok(target) => target,
            Err(error) => return Ok(ToolResult::error(error)),
        };
        let display_path = target.display_path.clone();

        let snapshot = match atomic_file::read_snapshot(&target).await {
            Ok(snapshot) => snapshot,
            Err(error) => {
                return Ok(ToolResult::error(format!(
                    "Failed to read {}: {error}",
                    display_path.display()
                )))
            }
        };
        let content = std::str::from_utf8(snapshot.bytes())
            .map_err(|e| format!("Failed to read {}: {e}", display_path.display()))?;
        after_snapshot().await;

        let mut matches = content.match_indices(old_string).peekable();
        let Some(&(first, _)) = matches.peek() else {
            return Ok(ToolResult::error(format!(
                "old_string not found in {}",
                display_path.display()
            )));
        };
        // A second match may start inside the first, but never inside a UTF-8 character.
        let mut tail = content[first..].chars();
        if !replace_all && tail.next().is_some() && tail.as_str().contains(old_string) {
            return Ok(ToolResult::error(format!(
                "old_string found more than once in {}. Provide more context to make the match unique, or use replace_all=true for non-overlapping matches.",
                display_path.display()
            )));
        }

        let mut new_content =
            String::with_capacity(content.len() - old_string.len() + new_string.len());
        let mut end = 0;
        let mut count = 0usize;
        for (start, matched) in matches {
            new_content.push_str(&content[end..start]);
            new_content.push_str(new_string);
            end = start + matched.len();
            count += 1;
            if !replace_all {
                break;
            }
        }
        new_content.push_str(&content[end..]);

        if let Err(error) =
            atomic_file::replace(&target, new_content.into_bytes(), Some(snapshot)).await
        {
            if matches!(error, ReplaceError::Conflict { .. }) {
                return Ok(ToolResult::error(error.to_string()));
            }
            return Ok(ToolResult::error(format!(
                "Failed to write {}: {error}",
                display_path.display()
            )));
        }

        Ok(ToolResult::success(format!(
            "Replaced {count} occurrence(s) in {}",
            display_path.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::Barrier;

    fn test_ctx(root: &Path) -> ToolContext {
        ToolContext::new(root.to_path_buf(), "test".to_string(), root.to_path_buf())
    }

    #[tokio::test]
    async fn edit_single_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        let ctx = test_ctx(directory.path());
        for (before, old, after) in [("文 hello world", "hello", "文 X world"), ("", "", "X")] {
            fs::write(&path, before).unwrap();
            let result = EditTool
                .invoke(
                    serde_json::json!({"path": path, "old_string": old, "new_string": "X"}),
                    &ctx,
                )
                .await
                .unwrap();
            assert!(!result.is_error, "{}", result.output);
            assert_eq!(fs::read(&path).unwrap(), after.as_bytes());
        }
    }

    #[tokio::test]
    async fn edit_not_found() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        fs::write(&path, "hello world").unwrap();

        let tool = EditTool;
        let result = tool
            .invoke(
                serde_json::json!({
                    "path": path.to_str().unwrap(),
                    "old_string": "xyz",
                    "new_string": "abc"
                }),
                &test_ctx(directory.path()),
            )
            .await
            .unwrap();
        assert!(result.is_error);
        assert!(result.output.contains("not found"));
        assert_eq!(fs::read_to_string(path).unwrap(), "hello world");
    }

    #[tokio::test]
    async fn edit_missing_file_returns_tool_error() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("missing.txt");

        let result = EditTool
            .invoke(
                serde_json::json!({
                    "path": path,
                    "old_string": "old",
                    "new_string": "new"
                }),
                &test_ctx(directory.path()),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.to_ascii_lowercase().contains("no such file"));
    }

    #[tokio::test]
    async fn edit_rejects_external_path_as_tool_error() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), "stable").unwrap();

        let result = EditTool
            .invoke(
                serde_json::json!({
                    "path": outside.path(),
                    "old_string": "stable",
                    "new_string": "blocked"
                }),
                &test_ctx(workspace.path()),
            )
            .await
            .unwrap();

        assert!(result.is_error);
        assert!(result.output.contains("escapes workspace root"));
        assert_eq!(fs::read_to_string(outside.path()).unwrap(), "stable");
    }

    #[tokio::test]
    async fn edit_ambiguous_without_replace_all() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        let ctx = test_ctx(directory.path());
        for (before, old) in [
            ("aaa bbb aaa", "aaa"),
            ("ababa", "aba"),
            ("前:哈哈哈", "哈哈"),
            ("   code", "  "),
            ("\n\n\n", "\n\n"),
            ("文", ""),
        ] {
            fs::write(&path, before).unwrap();
            let result = EditTool
                .invoke(
                    serde_json::json!({
                        "path": path, "old_string": old, "new_string": "X", "replace_all": false
                    }),
                    &ctx,
                )
                .await
                .unwrap();
            assert!(result.is_error, "accepted {old:?} in {before:?}");
            assert!(
                result.output.contains("more than once"),
                "{}",
                result.output
            );
            assert_eq!(fs::read(&path).unwrap(), before.as_bytes());
        }
    }

    #[tokio::test]
    async fn edit_replace_all() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.txt");
        let ctx = test_ctx(directory.path());
        for (before, old, after, count) in [
            ("aaa bbb aaa", "aaa", "X bbb X", 2),
            ("ababa", "aba", "Xba", 1),
            ("文", "", "X文X", 2),
        ] {
            fs::write(&path, before).unwrap();
            let result = EditTool
                .invoke(
                    serde_json::json!({
                        "path": path, "old_string": old, "new_string": "X", "replace_all": true
                    }),
                    &ctx,
                )
                .await
                .unwrap();
            assert!(!result.is_error, "{}", result.output);
            let expected = format!("Replaced {count} occurrence(s)");
            assert!(result.output.starts_with(&expected), "{}", result.output);
            assert_eq!(fs::read(&path).unwrap(), after.as_bytes());
        }
    }

    #[tokio::test]
    async fn edit_commit_rejects_an_intervening_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.rs");
        fs::write(&path, "hello world").unwrap();
        let workspace = super::super::workspace_fs::WorkspaceFs::new(directory.path()).unwrap();
        let target = workspace
            .prepare_write(directory.path(), "source.rs", false)
            .unwrap();
        let snapshot = atomic_file::read_snapshot(&target).await.unwrap();

        let error = atomic_file::replace_with_before_commit_for_test(
            &target,
            b"goodbye world",
            &snapshot,
            |target, _| {
                let replacement = target.with_extension("replacement");
                fs::write(&replacement, "intervening content")?;
                fs::rename(replacement, target)
            },
        )
        .unwrap_err();

        assert!(matches!(error, ReplaceError::Conflict { .. }));
        assert_eq!(fs::read_to_string(path).unwrap(), "intervening content");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_edit_invocations_allow_one_commit_and_one_conflict() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("source.rs");
        fs::write(&path, "hello world").unwrap();
        let tool = EditTool;
        let context = test_ctx(directory.path());
        let barrier = Arc::new(Barrier::new(2));

        let first_barrier = Arc::clone(&barrier);
        let second_barrier = Arc::clone(&barrier);
        let (first, second) = tokio::join!(
            tool.invoke_with_snapshot_hook(
                serde_json::json!({
                    "path": path,
                    "old_string": "hello",
                    "new_string": "first"
                }),
                &context,
                move || async move {
                    first_barrier.wait().await;
                },
            ),
            tool.invoke_with_snapshot_hook(
                serde_json::json!({
                    "path": path,
                    "old_string": "hello",
                    "new_string": "second"
                }),
                &context,
                move || async move {
                    second_barrier.wait().await;
                },
            ),
        );

        let first = first.unwrap();
        let second = second.unwrap();
        assert_eq!(
            usize::from(!first.is_error) + usize::from(!second.is_error),
            1
        );
        assert_eq!(
            usize::from(first.output.starts_with("Edit conflict:"))
                + usize::from(second.output.starts_with("Edit conflict:")),
            1
        );
        assert!(matches!(
            fs::read_to_string(path).unwrap().as_str(),
            "first world" | "second world"
        ));
    }
}
