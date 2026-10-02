use std::sync::Mutex;

use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolKind, ToolResult};

pub struct TodoTool {
    items: Mutex<Vec<TodoItem>>,
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
            items: Mutex::new(Vec::new()),
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

        let mut items = self.items.lock().map_err(|e| format!("lock error: {e}"))?;

        match action {
            "add" => {
                let text = params
                    .get("text")
                    .and_then(|v| v.as_str())
                    .ok_or("missing 'text' for add")?;
                let id = items.len() + 1;
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
}
