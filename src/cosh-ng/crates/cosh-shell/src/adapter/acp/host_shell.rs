//! host-shell v1 extension payload, frozen in design section 6.3.
//!
//! The envelope stays standard ACP: `_`-prefixed methods and `_meta` capability
//! advertisement. Only the Cosh payload is defined here. This module owns
//! validation and the mandatory-ask invariant; it never grants execution.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Capability key advertised under `_meta` when a foreground authority is wired.
pub const CAPABILITY_KEY: &str = "_cosh/shell";
/// Reverse request that asks the Shell to review and run one final action.
pub const EXECUTE_METHOD: &str = "_cosh/shell/v1/execute";
/// Notification that the Shell registered responsibility for one action.
pub const OWNED_METHOD: &str = "_cosh/shell/v1/owned";
/// Extension version negotiated independently of the ACP wire version.
pub const VERSION: u32 = 1;

/// Bound on identifier-shaped fields.
pub const MAX_ID_BYTES: usize = 256;
/// Bound on the final command text.
pub const MAX_COMMAND_BYTES: usize = 64 * 1024;
/// Bound on path-shaped fields.
pub const MAX_PATH_BYTES: usize = 4 * 1024;
/// Bound on model-facing content before truncation is recorded.
pub const MAX_LLM_CONTENT_BYTES: usize = 1024 * 1024;
/// A37: per-connection owned toolCallId capacity. A long-lived connection
/// must not accumulate owned action history without bound; at the cap, new
/// foreground actions are refused before ownership or dispatch, and already
/// owned ids stay non-replayable (no per-turn clear, no silent eviction).
pub const MAX_OWNED_TOOL_CALLS: usize = 4096;

/// Method not supported: the capability was never negotiated.
pub const CODE_METHOD_NOT_FOUND: i32 = -32601;
/// Invalid parameters: missing, mistyped, oversized or unknown field.
pub const CODE_INVALID_PARAMS: i32 = -32602;
/// Incompatible constraint: policy conflict, stale PTY or repeated action.
pub const CODE_CONSTRAINT: i32 = -32010;
/// Execution refused by the user or by local policy.
pub const CODE_REFUSED: i32 = -32011;
/// State unknown: ownership was lost before a terminal fact was observed.
pub const CODE_UNKNOWN: i32 = -32012;

/// One final action the Agent asks the Shell to own.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcpHostExecuteRequest {
    pub session_id: String,
    pub tool_call_id: String,
    pub command: String,
    pub cwd: String,
    #[serde(default)]
    pub expected_cwd: Option<String>,
    pub pty_instance: String,
    pub hook_requires_approval: bool,
    #[serde(default)]
    pub approval_id: Option<String>,
}

/// Kernel-evidenced input-wait facts, reusing the legacy metadata shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcpHostInputWait {
    pub waited_secs: u64,
    pub interrupted: bool,
}

/// Terminal result of one owned action, reusing legacy metadata field names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcpHostExecuteOutcome {
    pub command: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub signal: Option<String>,
    pub cwd: String,
    pub end_cwd: String,
    pub duration_ms: u64,
    pub output_ref: Option<String>,
    pub redaction_status: String,
    pub approval_id: Option<String>,
    pub tool_use_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_wait: Option<AcpHostInputWait>,
    #[serde(rename = "llmContent")]
    pub llm_content: String,
    /// Coverage fact set only when model-facing content was truncated.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub llm_content_truncated: bool,
}

/// Local review decision for one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcpHostDecision {
    /// Local policy allows the action; never satisfies a mandatory ask.
    Allow,
    /// A human decided to allow this exact action.
    HumanAllow,
    /// Refuse the action.
    Refuse,
}

/// A rejected extension request, carrying the frozen error code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpHostRefusal {
    /// One of the frozen host-shell error codes.
    pub code: i32,
    /// Developer-facing reason; never a claim that nothing ran.
    pub message: String,
}

impl AcpHostRefusal {
    /// Builds a refusal with a frozen code and its reason.
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

/// Admits a new foreground action only if its id is unseen and the connection
/// has owned-capacity headroom. Duplicate ids and a full owned set are both
/// constraint refusals; neither evicts nor clears existing ownership.
pub(super) fn owned_admission(
    owned: &HashSet<String>,
    tool_call_id: &str,
) -> Result<(), AcpHostRefusal> {
    if owned.contains(tool_call_id) {
        return Err(AcpHostRefusal::new(
            CODE_CONSTRAINT,
            format!("toolCallId {tool_call_id} was already owned on this connection"),
        ));
    }
    if owned.len() >= MAX_OWNED_TOOL_CALLS {
        return Err(AcpHostRefusal::new(
            CODE_CONSTRAINT,
            format!(
                "connection owned capacity reached ({MAX_OWNED_TOOL_CALLS}); refusing new foreground action"
            ),
        ));
    }
    Ok(())
}

/// Validates bounds and shapes that serde cannot express.
pub(super) fn validate(request: &AcpHostExecuteRequest) -> Result<(), AcpHostRefusal> {
    for (name, value) in [
        ("sessionId", request.session_id.as_str()),
        ("toolCallId", request.tool_call_id.as_str()),
        ("ptyInstance", request.pty_instance.as_str()),
    ] {
        if value.is_empty() {
            return Err(invalid(format!("{name} must not be empty")));
        }
        if value.len() > MAX_ID_BYTES {
            return Err(invalid(format!(
                "{name} exceeds the {MAX_ID_BYTES} byte limit"
            )));
        }
    }
    if request.command.trim().is_empty() {
        return Err(invalid("command must not be empty"));
    }
    if request.command.len() > MAX_COMMAND_BYTES {
        return Err(invalid(format!(
            "command exceeds the {MAX_COMMAND_BYTES} byte limit"
        )));
    }
    check_path("cwd", &request.cwd)?;
    if let Some(expected) = &request.expected_cwd {
        check_path("expectedCwd", expected)?;
    }
    if let Some(approval) = &request.approval_id {
        if approval.len() > MAX_ID_BYTES {
            return Err(invalid(format!(
                "approvalId exceeds the {MAX_ID_BYTES} byte limit"
            )));
        }
    }
    Ok(())
}

/// Applies the mandatory-ask invariant to one local decision.
///
/// A mandatory ask binds the final action: only a human decision can satisfy
/// it, so policy allow, trust and bulk consent cannot turn it into execution.
pub(super) fn review(
    request: &AcpHostExecuteRequest,
    decision: AcpHostDecision,
) -> Result<(), AcpHostRefusal> {
    match decision {
        AcpHostDecision::Refuse => Err(AcpHostRefusal::new(
            CODE_REFUSED,
            "local policy refused this action",
        )),
        AcpHostDecision::HumanAllow => Ok(()),
        AcpHostDecision::Allow if request.hook_requires_approval => Err(AcpHostRefusal::new(
            CODE_CONSTRAINT,
            "hookRequiresApproval requires a human decision; policy allow cannot satisfy it",
        )),
        AcpHostDecision::Allow => Ok(()),
    }
}

/// Bounds model-facing content and records truncation instead of claiming it
/// is complete.
pub(super) fn bound_content(mut outcome: AcpHostExecuteOutcome) -> AcpHostExecuteOutcome {
    if outcome.llm_content.len() <= MAX_LLM_CONTENT_BYTES {
        return outcome;
    }
    let mut end = MAX_LLM_CONTENT_BYTES;
    while end > 0 && !outcome.llm_content.is_char_boundary(end) {
        end -= 1;
    }
    outcome.llm_content.truncate(end);
    outcome.llm_content_truncated = true;
    outcome
}

fn check_path(name: &str, value: &str) -> Result<(), AcpHostRefusal> {
    if !value.starts_with('/') {
        return Err(invalid(format!("{name} must be an absolute path")));
    }
    if value.len() > MAX_PATH_BYTES {
        return Err(invalid(format!(
            "{name} exceeds the {MAX_PATH_BYTES} byte limit"
        )));
    }
    Ok(())
}

fn invalid(message: impl Into<String>) -> AcpHostRefusal {
    AcpHostRefusal::new(CODE_INVALID_PARAMS, message)
}

#[cfg(test)]
mod tests;
