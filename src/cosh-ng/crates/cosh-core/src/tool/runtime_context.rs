//! Read-only, on-demand access to the current cosh-ng runtime context.

use async_trait::async_trait;
use serde_json::{json, Value};

use super::{Tool, ToolContext, ToolKind, ToolResult};

pub struct RuntimeContextTool;

#[async_trait]
impl Tool for RuntimeContextTool {
    fn name(&self) -> &str {
        "runtime_context"
    }

    fn description(&self) -> &str {
        "Return read-only metadata for the current cosh-ng runtime, model, session, workspace, compaction state, and bound capabilities. Use this tool when another command needs the exact provider_session_id; never infer that ID from shell environment variables."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    async fn invoke(&self, _params: Value, ctx: &ToolContext) -> Result<ToolResult, String> {
        let payload = json!({
            "provider_session_id": ctx.session_id,
            "runtime": {
                "name": "cosh-ng",
                "version": env!("CARGO_PKG_VERSION"),
            },
            "model": ctx.runtime.model,
            "approval_mode": ctx.runtime.approval_mode,
            "workspace": {
                "cwd": ctx.cwd.to_string_lossy(),
                "project_root": ctx.project_root.to_string_lossy(),
            },
            "session": {
                "resumed": ctx.runtime.session_resumed,
            },
            "compaction": {
                "revision": ctx.runtime.compaction_revision,
                "active_projection": ctx.runtime.compacted_through.is_some(),
                "compacted_through": ctx.runtime.compacted_through,
            },
            "capabilities": {
                "tools": ctx.runtime.tools,
                "active_extensions": ctx.runtime.active_extensions,
            },
        });

        Ok(ToolResult::success(
            serde_json::to_string_pretty(&payload)
                .map_err(|error| format!("failed to serialize runtime context: {error}"))?,
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::tool::{SessionWorkspace, ToolRuntimeContext};

    #[tokio::test]
    async fn returns_current_runtime_metadata() {
        let project_root = PathBuf::from("/workspace/project");
        let context = ToolContext::with_runtime(
            project_root.join("src"),
            "provider-session-123".to_string(),
            project_root.clone(),
            SessionWorkspace::new(&project_root),
            ToolRuntimeContext {
                model: "qwen-test".to_string(),
                approval_mode: "recommend".to_string(),
                session_resumed: true,
                compaction_revision: 3,
                compacted_through: Some(42),
                tools: vec!["runtime_context".to_string(), "shell".to_string()],
                active_extensions: vec!["agent-sec-core".to_string()],
            },
        );

        let result = RuntimeContextTool
            .invoke(json!({}), &context)
            .await
            .expect("runtime context");
        let output: Value = serde_json::from_str(&result.output).expect("JSON output");

        assert!(!result.is_error);
        assert_eq!(output["provider_session_id"], "provider-session-123");
        assert_eq!(output["runtime"]["name"], "cosh-ng");
        assert_eq!(output["runtime"]["version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(output["model"], "qwen-test");
        assert!(output.get("provider").is_none());
        assert_eq!(output["approval_mode"], "recommend");
        assert_eq!(output["workspace"]["cwd"], "/workspace/project/src");
        assert_eq!(output["workspace"]["project_root"], "/workspace/project");
        assert_eq!(output["session"]["resumed"], true);
        assert_eq!(output["compaction"]["revision"], 3);
        assert_eq!(output["compaction"]["active_projection"], true);
        assert_eq!(output["compaction"]["compacted_through"], 42);
        assert_eq!(
            output["capabilities"]["tools"],
            json!(["runtime_context", "shell"])
        );
        assert_eq!(
            output["capabilities"]["active_extensions"],
            json!(["agent-sec-core"])
        );
        assert!(output["capabilities"].get("hooks_enabled").is_none());
    }

    #[test]
    fn is_read_only_and_takes_no_parameters() {
        let tool = RuntimeContextTool;

        assert_eq!(tool.kind(), ToolKind::ReadOnly);
        assert_eq!(tool.parameters_schema()["properties"], json!({}));
        assert_eq!(tool.parameters_schema()["additionalProperties"], false);
    }
}
