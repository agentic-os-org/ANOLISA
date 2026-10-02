//! Persistent project or user memory stored in context files.

use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};

use async_trait::async_trait;
use fs2::FileExt;
use serde_json::Value;

use crate::context::{context_path, ContextScope};

use super::{Tool, ToolContext, ToolKind, ToolResult};

const MEMORY_HEADER: &str = "## Saved Memories";

pub(super) struct SaveMemoryTool;

#[async_trait]
impl Tool for SaveMemoryTool {
    fn name(&self) -> &str {
        "save_memory"
    }

    fn description(&self) -> &str {
        "Save a concise fact to persistent project or global context for future sessions."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "fact": {
                    "type": "string",
                    "description": "A concise, self-contained fact to remember"
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "global"],
                    "description": "Storage scope (default: project)"
                }
            },
            "required": ["fact"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::FileEdit
    }

    async fn invoke(&self, params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let fact = params
            .get("fact")
            .and_then(Value::as_str)
            .ok_or("missing 'fact' parameter")?;
        let fact = normalize_fact(fact);
        if fact.is_empty() {
            return Ok(ToolResult::error("'fact' must not be empty"));
        }

        let scope = params
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or("project");
        let scope = ContextScope::parse(scope)
            .ok_or_else(|| "'scope' must be 'project' or 'global'".to_string())?;
        let path = context_path(scope, &ctx.project_root)
            .ok_or_else(|| "cannot resolve home directory for global memory".to_string())?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|error| format!("Failed to create {}: {error}", parent.display()))?;
        }
        let update_path = path.clone();
        let entry = format!("- {fact}");
        let outcome = tokio::task::spawn_blocking(move || update_memory_file(&update_path, &entry))
            .await
            .map_err(|error| format!("Memory update task failed: {error}"))??;

        let message = match outcome {
            SaveOutcome::Saved => format!("Saved memory to {}", path.display()),
            SaveOutcome::AlreadyExists => {
                format!("Memory already exists in {}", path.display())
            }
        };
        Ok(ToolResult::success(message))
    }
}

enum SaveOutcome {
    Saved,
    AlreadyExists,
}

fn update_memory_file(path: &std::path::Path, entry: &str) -> Result<SaveOutcome, String> {
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(path)
        .map_err(|error| format!("Failed to open {}: {error}", path.display()))?;
    file.lock_exclusive()
        .map_err(|error| format!("Failed to lock {}: {error}", path.display()))?;

    let mut existing = String::new();
    file.read_to_string(&mut existing)
        .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
    if existing.lines().any(|line| line.trim() == entry) {
        return Ok(SaveOutcome::AlreadyExists);
    }

    let updated = add_memory(&existing, entry);
    file.seek(SeekFrom::Start(0))
        .map_err(|error| format!("Failed to seek {}: {error}", path.display()))?;
    file.set_len(0)
        .map_err(|error| format!("Failed to truncate {}: {error}", path.display()))?;
    file.write_all(updated.as_bytes())
        .map_err(|error| format!("Failed to write {}: {error}", path.display()))?;
    file.sync_data()
        .map_err(|error| format!("Failed to sync {}: {error}", path.display()))?;
    Ok(SaveOutcome::Saved)
}

fn add_memory(existing: &str, entry: &str) -> String {
    let Some(header_index) = memory_header_index(existing) else {
        let separator = if existing.is_empty() || existing.ends_with("\n\n") {
            ""
        } else if existing.ends_with('\n') {
            "\n"
        } else {
            "\n\n"
        };
        return format!("{existing}{separator}{MEMORY_HEADER}\n{entry}\n");
    };

    let section_start = header_index + MEMORY_HEADER.len();
    let section_end = existing[section_start..]
        .find("\n## ")
        .map(|offset| section_start + offset)
        .unwrap_or(existing.len());
    let before = existing[..section_end].trim_end();
    let after = &existing[section_end..];
    if after.is_empty() {
        format!("{before}\n{entry}\n")
    } else {
        format!("{before}\n{entry}\n{after}")
    }
}

fn memory_header_index(existing: &str) -> Option<usize> {
    let mut offset = 0;
    for line in existing.split_inclusive('\n') {
        let content = line.trim_end_matches(['\r', '\n']);
        if content.trim() == MEMORY_HEADER {
            return content.find(MEMORY_HEADER).map(|index| offset + index);
        }
        offset += line.len();
    }
    None
}

fn normalize_fact(fact: &str) -> String {
    fact.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_start_matches(['-', '*'])
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn context(dir: &Path) -> ToolContext {
        ToolContext::new(dir.to_path_buf(), "test".to_string(), dir.to_path_buf())
    }

    #[tokio::test]
    async fn saves_project_memory_once() {
        let dir = tempfile::tempdir().unwrap();
        let params = serde_json::json!({
            "fact": "  - prefer   cargo nextest  ",
            "scope": "project"
        });

        let first = SaveMemoryTool
            .invoke(params.clone(), &context(dir.path()))
            .await
            .unwrap();
        let second = SaveMemoryTool
            .invoke(params, &context(dir.path()))
            .await
            .unwrap();

        assert!(!first.is_error);
        assert!(second.output.contains("already exists"));
        let content =
            std::fs::read_to_string(dir.path().join(".copilot-shell/CONTEXT.md")).unwrap();
        assert_eq!(content, "## Saved Memories\n- prefer cargo nextest\n");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn preserves_concurrent_memories() {
        let dir = tempfile::tempdir().unwrap();
        let project_root = dir.path().to_path_buf();
        let mut tasks = Vec::new();

        for index in 0..8 {
            let project_root = project_root.clone();
            tasks.push(tokio::spawn(async move {
                SaveMemoryTool
                    .invoke(
                        serde_json::json!({
                            "fact": format!("concurrent fact {index}"),
                            "scope": "project"
                        }),
                        &context(&project_root),
                    )
                    .await
                    .unwrap()
            }));
        }
        for task in tasks {
            assert!(!task.await.unwrap().is_error);
        }

        let content =
            std::fs::read_to_string(dir.path().join(".copilot-shell/CONTEXT.md")).unwrap();
        for index in 0..8 {
            assert!(content.contains(&format!("- concurrent fact {index}")));
        }
    }

    #[test]
    fn inserts_memory_before_the_next_section() {
        let existing = "# Context\n\n## Saved Memories\n- existing\n\n## Rules\nKeep this.";

        let updated = add_memory(existing, "- new fact");

        assert_eq!(
            updated,
            "# Context\n\n## Saved Memories\n- existing\n- new fact\n\n## Rules\nKeep this."
        );
    }

    #[test]
    fn ignores_saved_memories_text_outside_an_exact_header() {
        let existing = "### Saved Memories\nMention ## Saved Memories inline.";

        let updated = add_memory(existing, "- new fact");

        assert_eq!(
            updated,
            "### Saved Memories\nMention ## Saved Memories inline.\n\n\
             ## Saved Memories\n- new fact\n"
        );
    }
}
