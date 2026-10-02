//! Hosted checkpoint declaration for the private Gateway checkpoint profile.

use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolKind, ToolResult};

pub(super) struct WorkspaceCheckpointCreateTool;

#[async_trait]
impl Tool for WorkspaceCheckpointCreateTool {
    fn name(&self) -> &str {
        "workspace_checkpoint_create"
    }

    fn description(&self) -> &str {
        "Create one governed checkpoint for the workspace bound by COSH."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::HostedSideEffect
    }

    async fn invoke(&self, _params: Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        Ok(ToolResult::error(
            "workspace checkpoint execution is owned by the negotiated Gateway host",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_has_no_runtime_supplied_authority() {
        let schema = WorkspaceCheckpointCreateTool.parameters_schema();

        assert_eq!(schema["properties"], serde_json::json!({}));
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            WorkspaceCheckpointCreateTool.kind(),
            ToolKind::HostedSideEffect
        );
    }
}
