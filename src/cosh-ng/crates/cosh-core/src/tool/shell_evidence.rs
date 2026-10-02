use async_trait::async_trait;
use serde_json::Value;

use super::{Tool, ToolContext, ToolKind, ToolResult};

pub struct ShellEvidenceTool;

#[async_trait]
impl Tool for ShellEvidenceTool {
    fn name(&self) -> &str {
        "cosh_shell_evidence"
    }

    fn description(&self) -> &str {
        "List recorded shell command evidence or read a bounded output excerpt captured by cosh-shell. Use current shell tool results first; use this only for older ledger output or missing/truncated output coverage in the current cosh-shell evidence ledger."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "oneOf": [
                {
                    "properties": {
                        "action": { "const": "list_commands" },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 100,
                            "default": 20,
                            "description": "Maximum number of command facts to return"
                        },
                        "cursor": {
                            "type": ["string", "null"],
                            "description": "Opaque pagination cursor returned by list_commands"
                        }
                    },
                    "required": ["action"],
                    "additionalProperties": false
                },
                {
                    "properties": {
                        "action": { "const": "read_output" },
                        "output_id": {
                            "type": "string",
                            "description": "terminal-output://<shell-session-id>/<command-id>"
                        },
                        "direction": {
                            "type": "string",
                            "enum": ["head", "tail"],
                            "default": "tail"
                        },
                        "lines": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 300,
                            "default": 120
                        },
                        "bypass_recent_filter": {
                            "type": "boolean",
                            "default": false,
                            "description": "Rare retry escape hatch for read_output only. Leave false by default; set true only after a previous read_output returned already_delivered and the needed content is not in current context. Does not bypass ledger, redaction, or output caps."
                        }
                    },
                    "required": ["action", "output_id"],
                    "additionalProperties": false
                }
            ],
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["list_commands", "read_output"],
                    "description": "list_commands returns command facts/index; read_output returns a bounded output excerpt"
                },
                "limit": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 100,
                    "default": 20,
                    "description": "For list_commands only"
                },
                "cursor": {
                    "type": ["string", "null"],
                    "description": "Opaque pagination cursor for list_commands"
                },
                "output_id": {
                    "type": "string",
                    "description": "terminal-output://<shell-session-id>/<command-id>; required for read_output"
                },
                "direction": {
                    "type": "string",
                    "enum": ["head", "tail"],
                    "default": "tail"
                },
                "lines": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": 300,
                    "default": 120
                },
                "bypass_recent_filter": {
                    "type": "boolean",
                    "default": false,
                    "description": "Rare retry escape hatch for read_output only; normally omit."
                }
            },
            "required": ["action"]
        })
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ShellEvidence
    }

    async fn invoke(&self, _params: Value, _ctx: &ToolContext) -> Result<ToolResult, String> {
        Ok(ToolResult::error(
            "cosh_shell_evidence must be handled through the cosh-shell control protocol",
        ))
    }
}
