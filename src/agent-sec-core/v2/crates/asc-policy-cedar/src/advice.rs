//! Validated AGT-shaped requirements carried by matching permit policies.

use serde::{Deserialize, Serialize};

/// One processing requirement; authorization is still determined by Cedar.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Advice {
    /// Required processing behavior.
    pub verdict: AdviceVerdict,
    /// Stable reason code for callers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Explanation shown with warnings or confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Static replacement, present only for transform advice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transform: Option<Transform>,
}

/// Supported processing behaviors from the AGT advice contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdviceVerdict {
    /// Preserve a warning without blocking an allowed operation.
    Warn,
    /// Require explicit confirmation of the final operation.
    Escalate,
    /// Recheck a statically modified candidate before execution.
    Transform,
}

/// A JSON value replacement rooted at the execution target.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    /// Target field, such as `$target.args`.
    pub path: String,
    /// Complete replacement value for that field.
    pub value: serde_json::Value,
}

impl Advice {
    /// Parses one requirement without interpreting backend-specific target fields.
    ///
    /// # Errors
    /// Rejects invalid JSON, unknown fields, and inconsistent transform payloads.
    pub fn parse(value: &str) -> Result<Self, AdviceError> {
        let advice: Self = serde_json::from_str(value)?;
        if (advice.verdict == AdviceVerdict::Transform) != advice.transform.is_some() {
            return Err(AdviceError::InvalidTransform);
        }
        if advice.transform.as_ref().is_some_and(|transform| {
            !transform.path.starts_with("$target.") || transform.path.len() == "$target.".len()
        }) {
            return Err(AdviceError::InvalidTransform);
        }
        Ok(advice)
    }
}

/// An advice annotation cannot establish a usable processing contract.
#[derive(Debug, thiserror::Error)]
pub enum AdviceError {
    /// The annotation is not a supported JSON object.
    #[error("invalid advice JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// Transform advice requires exactly one target-rooted replacement.
    #[error("transform advice requires a replacement rooted at $target")]
    InvalidTransform,
}
