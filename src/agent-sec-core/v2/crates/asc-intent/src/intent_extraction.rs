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

/// Raw task plus its source label; the label does not prove an authorized identity.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    /// Non-empty source label, at most 256 bytes, without control characters.
    pub source_id: String,
    /// Unmodified task text, non-empty and at most 64 KiB.
    pub text: String,
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
    /// Task source ID, task text or model name is empty, has control characters or is oversized.
    #[error("task source ID, task text or model name is invalid")]
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
    if response.policy.len() > MAX_CONTENT_BYTES
        || response.unresolved.len() > 128
        || response.unresolved.iter().any(|reason| {
            reason.trim().is_empty() || reason.len() > 4096 || reason.chars().any(char::is_control)
        })
    {
        return Err(IntentError::InvalidDraft);
    }
    Ok(response)
}

fn check_deadline(deadline: Instant) -> Result<(), IntentError> {
    if Instant::now() >= deadline {
        Err(IntentError::DeadlineExceeded)
    } else {
        Ok(())
    }
}
