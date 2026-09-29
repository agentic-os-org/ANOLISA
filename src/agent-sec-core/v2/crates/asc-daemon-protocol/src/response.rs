use std::fmt;

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

/// Maximum encoded UTF-8 bytes in a public daemon error message.
pub const MAX_DAEMON_ERROR_MESSAGE_BYTES: usize = 256;

const ERROR_MESSAGE_SUFFIX: &str = "...";

/// Stable daemon-generated response correlation identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct RequestId(String);

impl RequestId {
    /// Creates a non-empty opaque request identity.
    ///
    /// # Errors
    /// Returns an error for an empty or whitespace-only identity.
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        if value.trim().is_empty() {
            Err("request ID must be a non-empty string")
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the wire value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for RequestId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for RequestId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Open, bounded, machine-readable daemon error code.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ErrorCode(String);

impl ErrorCode {
    /// Creates one lower-snake-case error code of at most 64 bytes.
    ///
    /// # Errors
    /// Returns an error when the code is not in canonical form.
    pub fn new(value: impl Into<String>) -> Result<Self, &'static str> {
        let value = value.into();
        let mut bytes = value.bytes();
        if value.len() > 64
            || !matches!(bytes.next(), Some(b'a'..=b'z'))
            || !bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            Err("error code must be a bounded lower_snake_case identifier")
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the canonical wire value.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for ErrorCode {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

/// Stable safe error returned by the daemon.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DaemonError {
    /// Machine-readable category.
    pub code: ErrorCode,
    /// Sanitized operator-facing explanation.
    #[serde(deserialize_with = "deserialize_error_message")]
    message: String,
}

impl DaemonError {
    /// Creates an error from a registered code and safe message.
    ///
    /// # Panics
    /// Panics when a programmer supplies a noncanonical code.
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: ErrorCode::new(code).expect("daemon error constants must be canonical"),
            message: bounded_error_message(message),
        }
    }

    /// Returns the bounded public explanation.
    pub fn message(&self) -> &str {
        &self.message
    }
}

fn bounded_error_message(message: &str) -> String {
    if message.len() <= MAX_DAEMON_ERROR_MESSAGE_BYTES {
        return message.to_owned();
    }

    let mut end = MAX_DAEMON_ERROR_MESSAGE_BYTES - ERROR_MESSAGE_SUFFIX.len();
    while !message.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ERROR_MESSAGE_SUFFIX}", &message[..end])
}

fn deserialize_error_message<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let message = String::deserialize(deserializer)?;
    if message.len() > MAX_DAEMON_ERROR_MESSAGE_BYTES {
        Err(D::Error::custom(
            "daemon error message must not exceed 256 bytes",
        ))
    } else {
        Ok(message)
    }
}

/// Successful first-version response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SuccessResponse<T> {
    /// Daemon-generated correlation identity.
    pub request_id: RequestId,
    /// Exact method result.
    pub result: T,
}

/// Failed first-version response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ErrorResponse {
    /// Daemon-generated correlation identity.
    pub request_id: RequestId,
    /// Stable structured failure.
    pub error: DaemonError,
}

/// Mutually exclusive `{requestId,result}` or `{requestId,error}` response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DaemonResponse<T = Value> {
    /// Successful application dispatch.
    Success(SuccessResponse<T>),
    /// Request, authorization, or application failure.
    Error(ErrorResponse),
}

impl<T> DaemonResponse<T> {
    /// Creates a successful response.
    pub fn success(request_id: RequestId, result: T) -> Self {
        Self::Success(SuccessResponse { request_id, result })
    }

    /// Creates a failed response.
    pub fn error(request_id: RequestId, code: &str, message: &str) -> Self {
        Self::Error(ErrorResponse {
            request_id,
            error: DaemonError::new(code, message),
        })
    }

    /// Returns the daemon-generated request identity.
    pub fn request_id(&self) -> &RequestId {
        match self {
            Self::Success(response) => &response.request_id,
            Self::Error(response) => &response.request_id,
        }
    }
}

/// Stable daemon error-code registry.
pub mod error_code {
    /// Malformed request envelope or method params.
    pub const INVALID_REQUEST: &str = "invalid_request";
    /// Successfully decoded method input failed application domain validation.
    pub const INVALID_ARGUMENT: &str = "invalid_argument";
    /// Method is not registered.
    pub const UNKNOWN_METHOD: &str = "unknown_method";
    /// Principal lacks authority.
    pub const PERMISSION_DENIED: &str = "permission_denied";
    /// Requested current record does not exist.
    pub const NOT_FOUND: &str = "not_found";
    /// Current state conflicts with the request.
    pub const CONFLICT: &str = "conflict";
    /// A bounded server resource is exhausted.
    pub const RESOURCE_EXHAUSTED: &str = "resource_exhausted";
    /// Service-owned deadline expired.
    pub const DEADLINE_EXCEEDED: &str = "deadline_exceeded";
    /// Service is temporarily unavailable.
    pub const UNAVAILABLE: &str = "unavailable";
    /// Internal failure with details withheld.
    pub const INTERNAL: &str = "internal";
}
