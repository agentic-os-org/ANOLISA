use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolKind, ToolResult};

pub struct TodoTool {
    state: Mutex<TodoState>,
}

struct TodoState {
    items: Vec<TodoItem>,
    // Callers may retain removed IDs, so never reuse them during this tool's lifetime.
    next_id: usize,
}

#[derive(Clone, serde::Serialize)]
struct TodoItem {
    id: usize,
    text: String,
    done: bool,
}

impl TodoTool {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TodoState {
                items: Vec::new(),
                next_id: 1,
            }),
        }
    }
}

#[async_trait]
impl Tool for TodoTool {
    fn name(&self) -> &str {
        "todo"
    }

    fn description(&self) -> &str {
        "Manage a simple todo list. Actions: add, list, done, remove."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "list", "done", "remove"],
                    "description": "The action to perform"
                },
                "text": {
                    "type": "string",
                    "description": "Text for 'add' action"
                },
                "id": {
                    "type": "integer",
                    "description": "Item ID for 'done' or 'remove' actions"
                }
            },
            "required": ["action"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Other
    }

    async fn invoke(&self, params: Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        let action = params
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or("missing 'action' parameter")?;

        let mut state = self.state.lock().map_err(|e| format!("lock error: {e}"))?;
        let TodoState { items, next_id } = &mut *state;

        match action {
            "add" => {
                let text = params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'text' for add")?;
                let id = *next_id;
                *next_id = id.checked_add(1).ok_or("todo item ID exhausted")?;
                items.push(TodoItem {
                    id,
                    text: text.to_string(),
                    done: false,
                });
                Ok(ToolResult::success(format!("Added item #{id}: {text}")))
            }
            "list" => {
                if items.is_empty() {
                    return Ok(ToolResult::success("No items."));
                }
                let text = items
                    .iter()
                    .map(|item| {
                        let status = if item.done { "x" } else { " " };
                        format!("[{status}] #{}: {}", item.id, item.text)
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(ToolResult::success(text))
            }
            "done" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing 'id' for done")? as usize;
                if let Some(item) = items.iter_mut().find(|i| i.id == id) {
                    item.done = true;
                    Ok(ToolResult::success(format!("Marked #{id} as done")))
                } else {
                    Ok(ToolResult::error(format!("Item #{id} not found")))
                }
            }
            "remove" => {
                let id = params
                    .get("id")
                    .and_then(|v| v.as_u64())
                    .ok_or("missing 'id' for remove")? as usize;
                let before = items.len();
                items.retain(|i| i.id != id);
                if items.len() < before {
                    Ok(ToolResult::success(format!("Removed #{id}")))
                } else {
                    Ok(ToolResult::error(format!("Item #{id} not found")))
                }
            }
            other => Ok(ToolResult::error(format!("Unknown action: {other}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_ctx() -> ToolContext {
        ToolContext::new(
            PathBuf::from("/tmp"),
            "test".to_string(),
            PathBuf::from("/tmp"),
        )
    }

    fn isolated_ctx() -> (tempfile::TempDir, ToolContext) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolContext::new(
            dir.path().to_path_buf(),
            "test".to_string(),
            dir.path().to_path_buf(),
        );
        (dir, ctx)
    }

    #[tokio::test]
    async fn todo_add_and_list() {
        let tool = TodoTool::new();
        let ctx = test_ctx();

        let r = tool
            .invoke(
                serde_json::json!({"action": "add", "text": "buy milk"}),
                &ctx,
            )
            .await
            .unwrap();
        assert!(!r.is_error);
        assert!(r.output.contains("buy milk"));

        let r = tool
            .invoke(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(r.output.contains("buy milk"));
        assert!(r.output.contains("[ ]"));
    }

    #[tokio::test]
    async fn todo_done_and_remove() {
        let tool = TodoTool::new();
        let ctx = test_ctx();

        tool.invoke(serde_json::json!({"action": "add", "text": "task1"}), &ctx)
            .await
            .unwrap();

        let r = tool
            .invoke(serde_json::json!({"action": "done", "id": 1}), &ctx)
            .await
            .unwrap();
        assert!(!r.is_error);

        let r = tool
            .invoke(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(r.output.contains("[x]"));

        let r = tool
            .invoke(serde_json::json!({"action": "remove", "id": 1}), &ctx)
            .await
            .unwrap();
        assert!(!r.is_error);

        let r = tool
            .invoke(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert!(r.output.contains("No items"));
    }

    #[tokio::test]
    async fn todo_deletion_keeps_other_item_ids_distinct() {
        let tool = TodoTool::new();
        let (_dir, ctx) = isolated_ctx();
        for text in ["first", "second"] {
            tool.invoke(serde_json::json!({"action": "add", "text": text}), &ctx)
                .await
                .unwrap();
        }
        tool.invoke(serde_json::json!({"action": "remove", "id": 1}), &ctx)
            .await
            .unwrap();

        let added = tool
            .invoke(serde_json::json!({"action": "add", "text": "third"}), &ctx)
            .await
            .unwrap();
        assert_eq!(added.output, "Added item #3: third");
        tool.invoke(serde_json::json!({"action": "done", "id": 2}), &ctx)
            .await
            .unwrap();
        let listed = tool
            .invoke(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert_eq!(listed.output, "[x] #2: second\n[ ] #3: third");

        tool.invoke(serde_json::json!({"action": "remove", "id": 2}), &ctx)
            .await
            .unwrap();
        let listed = tool
            .invoke(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert_eq!(listed.output, "[ ] #3: third");
    }

    #[tokio::test]
    async fn todo_removed_ids_stay_invalid_after_list_becomes_empty() {
        let tool = TodoTool::new();
        let (_dir, ctx) = isolated_ctx();
        tool.invoke(serde_json::json!({"action": "add", "text": "first"}), &ctx)
            .await
            .unwrap();
        tool.invoke(serde_json::json!({"action": "remove", "id": 1}), &ctx)
            .await
            .unwrap();
        let added = tool
            .invoke(serde_json::json!({"action": "add", "text": "second"}), &ctx)
            .await
            .unwrap();
        assert_eq!(added.output, "Added item #2: second");

        for action in ["done", "remove"] {
            let stale = tool
                .invoke(serde_json::json!({"action": action, "id": 1}), &ctx)
                .await
                .unwrap();
            assert!(stale.is_error);
            assert_eq!(stale.output, "Item #1 not found");
        }
        let listed = tool
            .invoke(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert_eq!(listed.output, "[ ] #2: second");
    }

    #[tokio::test]
    async fn todo_failed_additions_preserve_ids_and_items() {
        let tool = TodoTool::new();
        let (_dir, ctx) = isolated_ctx();
        let invalid = tool
            .invoke(serde_json::json!({"action": "add"}), &ctx)
            .await;
        assert_eq!(invalid.err().as_deref(), Some("missing 'text' for add"));
        let added = tool
            .invoke(serde_json::json!({"action": "add", "text": "first"}), &ctx)
            .await
            .unwrap();
        assert_eq!(added.output, "Added item #1: first");

        tool.state.lock().unwrap().next_id = usize::MAX;
        let exhausted = tool
            .invoke(
                serde_json::json!({"action": "add", "text": "overflow"}),
                &ctx,
            )
            .await;
        assert_eq!(exhausted.err().as_deref(), Some("todo item ID exhausted"));
        assert_eq!(tool.state.lock().unwrap().next_id, usize::MAX);
        let listed = tool
            .invoke(serde_json::json!({"action": "list"}), &ctx)
            .await
            .unwrap();
        assert_eq!(listed.output, "[ ] #1: first");
    }
}
