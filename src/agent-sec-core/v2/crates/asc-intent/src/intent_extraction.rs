//! Generates unconfirmed Cedar text from a task and the shared schema; reads no files or path metadata.

use std::time::{Duration, Instant};

use asc_model_client::provider::{MAX_CONTENT_BYTES, ModelProvider, ProviderError};
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest as _, Sha256};
use thiserror::Error;

use crate::CEDAR_SCHEMA;

/// Upper bound for one model call; an earlier caller deadline takes precedence.
pub const INTENT_BUDGET: Duration = Duration::from_secs(30);
/// Version of the current Cedar generation prompt.
pub const PROMPT_VERSION: &str = "cedar-policy@1";
/// Version of the direct Cedar draft format.
pub const DRAFT_VERSION: &str = "asc.intent.draft.v1";

const SYSTEM_PROMPT: &str = include_str!("../prompts/policy-generation.md");
const COMMAND_SYSTEM_PROMPT: &str = include_str!("../prompts/command-proposal.md");

/// Raw task plus its source label; the label does not prove an authorized identity.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    /// Non-empty source label, at most 256 bytes, without control characters.
    pub source_id: String,
    /// Unmodified task text, non-empty and at most 64 KiB.
    pub text: String,
}

/// Raw input of a single hook command; cwd only supplies path context, not an authorization scope.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandInput {
    /// Raw shell command; non-empty, at most 64 KiB, without NUL.
    pub command: String,
    /// Canonical absolute working directory, at most 4096 bytes; no filesystem lookup happens.
    pub cwd: String,
}

/// UTF-8 byte limit of one command.
pub const MAX_COMMAND_BYTES: usize = 64 * 1024;
/// UTF-8 byte limit of a working directory.
pub const MAX_CWD_BYTES: usize = 4096;

impl CommandInput {
    /// Checks the input shape; it runs no command, expands no variable and reads no path metadata.
    ///
    /// # Errors
    /// Returns an error when the command is empty, over a limit or contains NUL, or when cwd is
    /// not a canonical absolute path.
    pub fn validate(&self) -> Result<(), IntentError> {
        if self.command.trim().is_empty()
            || self.command.len() > MAX_COMMAND_BYTES
            || self.command.contains('\0')
            || self.cwd.len() > MAX_CWD_BYTES
            || !crate::policy_validation::absolute_path(&self.cwd)
        {
            return Err(IntentError::InvalidInput);
        }
        Ok(())
    }
}

/// The command prompt's output wrapper; `caveats` records gaps that do not block a candidate.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CommandResponse {
    /// Raw Cedar text; empty only when the command triggers no file, process or network action.
    pub policy: String,
    /// Ambiguities that make the whole command inexpressible; non-empty means no candidate.
    pub unresolved: Vec<String>,
    /// Known coverage gaps of an otherwise usable candidate, such as an unresolved PATH lookup.
    #[serde(default)]
    pub caveats: Vec<String>,
}

/// `rejection` replays the local check that refused the previous answer, so a retry is not blind.
pub(crate) fn generate_command_response(
    provider: &dyn ModelProvider,
    model: &str,
    input: &CommandInput,
    deadline: Instant,
    rejection: Option<&str>,
) -> Result<CommandResponse, IntentError> {
    check_deadline(deadline)?;
    input.validate()?;
    if model.trim().is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        return Err(IntentError::InvalidInput);
    }
    let mut payload = serde_json::to_string(input).map_err(|_| IntentError::InvalidInput)?;
    if let Some(rejection) = rejection {
        payload =
            format!("{payload}\n上次输出被本地校验拒绝：{rejection}。请只输出修正后的 JSON。");
    }
    let system = format!("{COMMAND_SYSTEM_PROMPT}\n```cedar\n{CEDAR_SCHEMA}\n```\n");
    let content = provider.complete(model, &system, &payload, deadline)?;
    check_deadline(deadline)?;
    let response = validate_command_response(&content)?;
    check_deadline(deadline)?;
    Ok(response)
}

/// The model's only output wrapper; carries no approval state or trusted identity claims.
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyResponse {
    /// Raw Cedar text that no program check has passed yet; may be empty.
    pub policy: String,
    /// Ambiguities the model reported; not a completeness proof.
    pub unresolved: Vec<String>,
}

/// Keeps the raw model text and task source so it can be compared with the final candidate.
#[derive(Serialize)]
pub struct PolicyDraft {
    format_version: &'static str,
    status: &'static str,
    prompt_version: &'static str,
    source: SourceRecord,
    #[serde(flatten)]
    response: PolicyResponse,
}

#[derive(Serialize)]
struct SourceRecord {
    source_id: String,
    kind: &'static str,
    byte_length: usize,
    sha256: String,
}

impl PolicyDraft {
    /// Raw model Cedar text; it does not mean the policy is approved.
    pub fn policy(&self) -> &str {
        &self.response.policy
    }

    /// Requirements the model reported as unresolved; the list cannot prove the rest were met.
    pub fn unresolved(&self) -> &[String] {
        &self.response.unresolved
    }
}

/// Generation failures expose no task body, raw model response or secret.
#[derive(Debug, Error)]
pub enum IntentError {
    /// The task, command context or model name violates the input constraints.
    #[error("intent input or model name is invalid")]
    InvalidInput,
    /// The effective deadline had already passed when the result arrived.
    #[error("intent generation deadline exceeded")]
    DeadlineExceeded,
    /// The model service is unavailable or the request failed.
    #[error("intent generation provider failed: {0}")]
    Provider(#[from] ProviderError),
    /// The model response exceeds its size budget.
    #[error("model response exceeds its content budget")]
    InvalidResponse,
    /// The output does not match the policy/unresolved wrapper or its field budgets.
    #[error("policy draft has invalid fields")]
    InvalidDraft,
}

/// Generates a Cedar draft directly; neither enforces the policy nor proves task completeness.
///
/// The model receives only the task and the embedded schema; no file contents are read and no
/// path metadata is injected.
/// A blocking provider cannot be interrupted here, but results past the deadline are rejected.
///
/// # Errors
/// Returns a redacted error when input, provider, deadline or response validation fails.
pub fn generate_policy_draft(
    provider: &dyn ModelProvider,
    model: &str,
    task: &TaskInput,
    deadline: Instant,
) -> Result<PolicyDraft, IntentError> {
    let deadline = deadline.min(Instant::now() + INTENT_BUDGET);
    check_deadline(deadline)?;
    if task.source_id.is_empty()
        || task.source_id.len() > 256
        || task.source_id.chars().any(char::is_control)
        || task.text.trim().is_empty()
        || task.text.len() > 64 * 1024
        || model.trim().is_empty()
        || model.len() > 256
        || model.chars().any(char::is_control)
    {
        return Err(IntentError::InvalidInput);
    }
    let input = json!({
        "task": {"source_id": task.source_id, "text": task.text},
    })
    .to_string();
    let system =
        format!("{SYSTEM_PROMPT}\n## Shared type contract\n```cedar\n{CEDAR_SCHEMA}\n```\n");
    let content = provider.complete(model, &system, &input, deadline)?;
    check_deadline(deadline)?;
    let response = validate_policy_response(&content)?;
    check_deadline(deadline)?;
    Ok(PolicyDraft {
        format_version: DRAFT_VERSION,
        status: "DRAFT",
        prompt_version: PROMPT_VERSION,
        source: SourceRecord {
            source_id: task.source_id.clone(),
            kind: "task_text",
            byte_length: task.text.len(),
            sha256: format!("sha256:{:x}", Sha256::digest(task.text.as_bytes())),
        },
        response,
    })
}

/// Validates the model JSON wrapper; the candidate validation layer handles Cedar syntax and types.
///
/// # Errors
/// Rejects missing, null, duplicate or unknown fields, plus oversized, blank or control-character reasons.
pub fn validate_policy_response(content: &str) -> Result<PolicyResponse, IntentError> {
    if content.len() > MAX_CONTENT_BYTES {
        return Err(IntentError::InvalidResponse);
    }
    let response: PolicyResponse =
        serde_json::from_str(content).map_err(|_| IntentError::InvalidDraft)?;
    if response.policy.len() > MAX_CONTENT_BYTES || !reasons_valid(&response.unresolved) {
        return Err(IntentError::InvalidDraft);
    }
    Ok(response)
}

/// Validates the command wrapper, which also carries non-blocking caveats.
fn validate_command_response(content: &str) -> Result<CommandResponse, IntentError> {
    if content.len() > MAX_CONTENT_BYTES {
        return Err(IntentError::InvalidResponse);
    }
    let response: CommandResponse =
        serde_json::from_str(content).map_err(|_| IntentError::InvalidDraft)?;
    if response.policy.len() > MAX_CONTENT_BYTES
        || !reasons_valid(&response.unresolved)
        || !reasons_valid(&response.caveats)
    {
        return Err(IntentError::InvalidDraft);
    }
    Ok(response)
}

/// Bounds model-reported reasons and caveats, so a shared budget keeps the daemon's input checks
/// and the CLI's revalidation in step; see `docs/design/DAEMON_PROTOCOL_V1_zh.md` §19.
///
/// Returns false for more than 128 items, blank items, items longer than 4096 bytes, or items
/// containing control characters.
pub fn reasons_valid(reasons: &[String]) -> bool {
    reasons.len() <= 128
        && reasons.iter().all(|reason| {
            !reason.trim().is_empty()
                && reason.len() <= 4096
                && !reason.chars().any(char::is_control)
        })
}

fn check_deadline(deadline: Instant) -> Result<(), IntentError> {
    if Instant::now() >= deadline {
        Err(IntentError::DeadlineExceeded)
    } else {
        Ok(())
    }
}
