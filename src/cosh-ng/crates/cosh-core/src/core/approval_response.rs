use serde::{Deserialize, Deserializer};
use serde_json::Value;

use super::{
    hosted_checkpoint_error, hosted_checkpoint_result, ApprovalResult, CoshCore, ToolKind,
};
use crate::protocol::{
    HostExecutedCheckpointCreateResult, HostExecutedCheckpointError, InputMessage,
    ShellControlRequest,
};

#[derive(Deserialize)]
struct RoutingHeader {
    #[serde(rename = "type")]
    kind: String,
    request_id: Option<Value>,
    response: Option<RoutingResponse>,
}

#[derive(Deserialize)]
struct RoutingResponse {
    request_id: Option<String>,
}

pub(super) enum ApprovalFrameOutcome {
    Ignore,
    Receipt,
    Interrupted,
    Decision(ApprovalResult),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptFrame {
    #[serde(rename = "type")]
    _kind: String,
    request_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalFrame {
    #[serde(rename = "type")]
    kind: String,
    response: ApprovalPayload,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovalPayload {
    subtype: String,
    request_id: String,
    response: ApprovalBody,
}

#[derive(Deserialize)]
#[serde(tag = "behavior", deny_unknown_fields)]
enum ApprovalBody {
    #[serde(rename = "allow")]
    Allow {
        #[serde(rename = "message")]
        _message: Option<String>,
        #[serde(rename = "updatedPermissions")]
        _updated_permissions: Option<Vec<Value>>,
        #[serde(rename = "toolUseID")]
        _tool_use_id: Option<String>,
    },
    #[serde(rename = "deny")]
    Deny { message: Option<String> },
    #[serde(rename = "host_executed_shell")]
    Shell { result: Box<ShellResult> },
    #[serde(rename = "host_executed_checkpoint_create")]
    Checkpoint {
        #[serde(rename = "checkpointResult")]
        result: HostExecutedCheckpointCreateResult,
    },
    #[serde(rename = "host_executed_checkpoint_error")]
    CheckpointError {
        #[serde(rename = "checkpointError")]
        error: HostExecutedCheckpointError,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ShellResult {
    #[serde(rename = "llmContent")]
    content: String,
    #[serde(rename = "returnDisplay")]
    _return_display: Option<String>,
    metadata: Option<ShellMetadata>,
}

#[derive(Deserialize)]
struct ShellMetadata {
    exit_code: Option<Value>,
    // Absence supports legacy senders; explicit null cannot confirm completion.
    #[serde(default, deserialize_with = "present_json")]
    status: Option<Value>,
    signal: Option<Value>,
    input_wait: Option<InputWaitMetadata>,
    #[serde(rename = "approval_id")]
    _approval_id: Option<String>,
    #[serde(rename = "tool_use_id")]
    _tool_use_id: Option<String>,
}

#[derive(Deserialize)]
struct InputWaitMetadata {
    interrupted: Option<Value>,
}

fn present_json<'de, D: Deserializer<'de>>(input: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(input).map(Some)
}

fn unconfirmed(reason: &str) -> ApprovalResult {
    ApprovalResult::Unconfirmed(Some(reason.to_string()))
}

impl CoshCore {
    pub(super) fn normalize_approval_frame(
        &self,
        line: &str,
        expected_request_id: &str,
        kind: Option<ToolKind>,
    ) -> ApprovalFrameOutcome {
        use ApprovalFrameOutcome::{Decision, Ignore, Interrupted, Receipt};

        let header: RoutingHeader = match serde_json::from_str(line) {
            Ok(header) => header,
            Err(error) => return Decision(rejected_frame(expected_request_id, &error)),
        };
        let nested_id = header
            .response
            .as_ref()
            .and_then(|response| response.request_id.as_deref());
        if header.kind == "control_response" {
            match nested_id {
                Some(id) if !id.is_empty() && id != expected_request_id => return Ignore,
                Some(id) if id == expected_request_id => {}
                _ => return Decision(unconfirmed("approval response has no valid request_id")),
            }
        } else if header.kind == "approval_receipt" {
            return match serde_json::from_str::<ReceiptFrame>(line) {
                Ok(receipt) if receipt.request_id == expected_request_id => Receipt,
                Ok(receipt) if !receipt.request_id.is_empty() => Ignore,
                Ok(_) => Decision(unconfirmed("approval receipt has no valid request_id")),
                Err(error) => Decision(rejected_frame(expected_request_id, &error)),
            };
        } else {
            let owns_request = header.request_id.as_ref().and_then(Value::as_str)
                == Some(expected_request_id)
                || nested_id == Some(expected_request_id);
            return match serde_json::from_str::<InputMessage>(line) {
                Ok(InputMessage::ControlRequest {
                    request: ShellControlRequest::Interrupt,
                    ..
                }) => Interrupted,
                Ok(_) if !owns_request => Ignore,
                Ok(_) => Decision(unconfirmed(
                    "approval response has an unexpected frame type",
                )),
                Err(error) => Decision(rejected_frame(expected_request_id, &error)),
            };
        }

        let frame: ApprovalFrame = match serde_json::from_str(line) {
            Ok(frame) => frame,
            Err(error) => return Decision(rejected_frame(expected_request_id, &error)),
        };
        if frame.kind != "control_response"
            || frame.response.request_id != expected_request_id
            || frame.response.subtype != "success"
        {
            return Decision(unconfirmed("approval response envelope is not successful"));
        }
        Decision(match frame.response.response {
            ApprovalBody::Allow { .. } => {
                if self.execution_profile.is_brokered() && kind == Some(ToolKind::HostedSideEffect)
                {
                    unconfirmed("brokered side effect rejected generic allow; a typed Gateway result is required")
                } else {
                    ApprovalResult::Allowed
                }
            }
            ApprovalBody::Deny { message } => ApprovalResult::Denied(message),
            ApprovalBody::Shell { result } => {
                if kind != Some(ToolKind::ShellExec) || self.execution_profile.is_brokered() {
                    unconfirmed("host_executed_shell is only valid for shell tools")
                } else {
                    normalize_shell(*result)
                }
            }
            ApprovalBody::Checkpoint { result } => {
                if !self.execution_profile.hosts_checkpoint()
                    || kind != Some(ToolKind::HostedSideEffect)
                {
                    unconfirmed("unknown response")
                } else {
                    match hosted_checkpoint_result(result) {
                        Ok(result) => ApprovalResult::HostExecutedCheckpoint { result },
                        Err(reason) => ApprovalResult::Unconfirmed(Some(reason)),
                    }
                }
            }
            ApprovalBody::CheckpointError { error } => {
                if !self.execution_profile.hosts_checkpoint()
                    || kind != Some(ToolKind::HostedSideEffect)
                {
                    unconfirmed("unknown response")
                } else {
                    match hosted_checkpoint_error(error) {
                        Ok(result) => ApprovalResult::HostExecutedCheckpoint { result },
                        Err(reason) => ApprovalResult::Unconfirmed(Some(reason)),
                    }
                }
            }
        })
    }
}

fn rejected_frame(request_id: &str, error: &serde_json::Error) -> ApprovalResult {
    tracing::warn!(request_id, error = ?error, "approval response could not be normalized");
    unconfirmed("unknown response")
}

fn normalize_shell(result: ShellResult) -> ApprovalResult {
    let exit_code = match result.metadata {
        None => None,
        Some(metadata) => {
            let exit_code = match metadata.exit_code {
                None => None,
                Some(value) => match value.as_i64().and_then(|code| i32::try_from(code).ok()) {
                    Some(code) => Some(code),
                    None => {
                        return unconfirmed("host_executed_shell response has invalid exit_code")
                    }
                },
            };
            if exit_code == Some(0)
                && (metadata
                    .status
                    .is_some_and(|status| status.as_str() != Some("completed"))
                    || metadata.signal.is_some()
                    || metadata.input_wait.is_some_and(|facts| {
                        facts.interrupted.and_then(|value| value.as_bool()) != Some(false)
                    }))
            {
                return unconfirmed(
                    "host_executed_shell response has contradictory terminal evidence",
                );
            }
            exit_code
        }
    };
    ApprovalResult::HostExecutedShell {
        llm_content: result.content,
        exit_code,
    }
}
