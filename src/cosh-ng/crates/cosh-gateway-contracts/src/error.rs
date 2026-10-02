//! Bounded and machine-readable contract errors.

use serde::{de, Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use crate::common::{BoundedOpaque, BoundedStringError, BoundedText};

/// Maximum UTF-8 byte length of a stable machine-readable error code.
pub const MAX_ERROR_CODE_BYTES: usize = 64;

/// Failure returned when an error code is empty, oversized, or unstable text.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ErrorCodeError {
    /// An error code must identify a concrete failure.
    #[error("error code must not be empty")]
    Empty,
    /// Codes are capped to keep transport and storage records predictable.
    #[error("error code exceeds the {MAX_ERROR_CODE_BYTES}-byte limit")]
    TooLong,
    /// Codes use lowercase ASCII snake-case for cross-language stability.
    #[error("error code must use lowercase ASCII letters, digits, and underscores")]
    InvalidCharacter,
}

/// Stable machine-readable error code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ErrorCode(String);

impl ErrorCode {
    /// Parses a stable lowercase snake-case error code.
    pub fn parse(value: impl Into<String>) -> Result<Self, ErrorCodeError> {
        let value = value.into();
        if value.is_empty() {
            return Err(ErrorCodeError::Empty);
        }
        if value.len() > MAX_ERROR_CODE_BYTES {
            return Err(ErrorCodeError::TooLong);
        }
        if !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(ErrorCodeError::InvalidCharacter);
        }
        Ok(Self(value))
    }

    /// Returns the stable machine-readable code.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Serialize for ErrorCode {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(de::Error::custom)
    }
}

/// Stable category used by transports and retry policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCategory {
    /// Input is malformed or violates a contract precondition.
    InvalidRequest,
    /// State or idempotency preconditions conflict.
    Conflict,
    /// Requested durable entity does not exist.
    NotFound,
    /// The actor is unauthenticated or lacks access.
    Unauthorized,
    /// OS or capability policy denied the request.
    PolicyDenied,
    /// The selected Agent Runtime is unavailable.
    RuntimeUnavailable,
    /// A transport failed before a domain result was known.
    Transport,
    /// Durable state could not be read or committed.
    Storage,
    /// The operation was cancelled.
    Cancelled,
    /// An invariant failed without a safe public diagnostic.
    Internal,
}

/// Safe bounded failure exposed by domain and transport envelopes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractError {
    /// Stable machine-readable code.
    pub code: ErrorCode,
    /// Broad failure category.
    pub category: ErrorCategory,
    /// Whether retry policy may consider the operation again.
    pub retryable: bool,
    /// Redacted message safe for a caller.
    pub safe_message: BoundedText,
    /// Optional minimum delay before retrying.
    pub retry_after_ms: Option<u64>,
    /// Optional opaque reference to separately governed diagnostic evidence.
    pub details_ref: Option<BoundedOpaque>,
}

impl ContractError {
    /// Constructs a bounded error without diagnostic details.
    pub fn new(
        code: impl Into<String>,
        category: ErrorCategory,
        retryable: bool,
        safe_message: impl Into<String>,
    ) -> Result<Self, ContractErrorBuildError> {
        Ok(Self {
            code: ErrorCode::parse(code)?,
            category,
            retryable,
            safe_message: BoundedText::new(safe_message)?,
            retry_after_ms: None,
            details_ref: None,
        })
    }
}

/// Failure returned while constructing a bounded contract error.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ContractErrorBuildError {
    /// The stable code is invalid.
    #[error(transparent)]
    Code(#[from] ErrorCodeError),
    /// The safe message violates its bounded-string contract.
    #[error(transparent)]
    SafeMessage(#[from] BoundedStringError),
}
